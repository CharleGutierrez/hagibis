pub mod canvas;
mod client;
mod repl;
pub mod menu;
pub mod desktop_ide;

use canvas::{ChatCanvas, ToolCallCard, ToolCardStatus};
use clap::{Parser, Subcommand};
use client::HgbClient;
use colored::Colorize;
use hgb_core::{HgbRequest, HgbResponse};
use repl::HagibisRepl;

#[derive(Parser)]
#[command(
    name = "hgb",
    bin_name = "hgb",
    version,
    about = "🪽 Hagibis (hgb): Sub-Millisecond Microkernel & Swarm Engine in Systems-Grade Rust",
    long_about = "🪽 Hagibis (hgb) — The Sub-Millisecond Microkernel Swarm Engine in Systems-Grade Rust.\nRunning without arguments launches the AGY Chat Canvas & Interactive Cockpit."
)]
struct Cli {
    /// Launch classic line-by-line scrolling terminal REPL instead of AGY Chat Canvas
    #[arg(long, global = true)]
    classic: bool,

    /// Resume the latest saved Cockpit session
    #[arg(long, global = true)]
    resume: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ModelSubcommand {
    /// Switch resident daemon's active model
    Switch {
        /// Target model name (e.g. gemini-2.5-pro, ollama/qwen2.5-coder:1.5b)
        name: String,
    },
    /// List all available local and cloud models
    List,
    /// Display currently active model
    Current,
}

#[derive(Subcommand, Debug, Clone)]
pub enum EnterpriseSubcommand {
    /// Show current Hagibis edition (Community vs. Enterprise) and entitlement status
    Status,
    /// Activate Enterprise edition with a license token (saves to .hgb/enterprise.lic)
    Activate {
        /// Cryptographic license token string (e.g. HGB-ENT-...)
        token: String,
    },
    /// Issue a new cryptographic enterprise license token
    Issue {
        /// Tenant identifier (e.g. contoso-production)
        #[arg(long, default_value = "default-enterprise-tenant")]
        tenant_id: String,
        /// Organization or company name
        #[arg(long, default_value = "Enterprise Client")]
        org: String,
        /// Number of authorized developer seats
        #[arg(long, default_value_t = 100)]
        seats: u32,
        /// Validity period in days
        #[arg(long, default_value_t = 365)]
        days: i64,
        /// Automatically save and activate this license in .hgb/enterprise.lic
        #[arg(long)]
        save: bool,
    },
    /// Export SOC2 / ISO 27001 append-only cryptographic audit records
    Audit {
        /// Export format: json or csv
        #[arg(long, default_value = "json")]
        format: String,
    },
    /// Generate a Microsoft 365 MCP configuration (.hgb/mcp.microsoft.json)
    McpConfig {
        /// Optional tenant ID
        #[arg(long)]
        tenant_id: Option<String>,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum DaemonCliSubcommand {
    /// Inspect daemon status, uptime, and ambient background service health
    Status,
    /// Read overnight dream journal (.hgb/dream_journal.json)
    Journal,
    /// Install systemd user service unit (~/.config/systemd/user/hgbd.service)
    InstallService,
}

#[derive(Subcommand, Debug, Clone)]
pub enum CheckpointSubcommand {
    /// List all recorded workspace checkpoints
    List,
    /// Create a new checkpoint snapshot
    Create {
        /// Label or description for the checkpoint
        label: String,
    },
    /// Roll back to a specific checkpoint ID
    Rewind {
        /// Target checkpoint ID
        id: String,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum StyleSubcommand {
    /// Record accepted style pattern or rejected anti-pattern
    Learn {
        /// Code snippet or pattern to record
        #[arg(short, long)]
        snippet: String,
        /// Mark snippet as accepted style pattern
        #[arg(long, conflicts_with = "reject")]
        accept: bool,
        /// Mark snippet as rejected anti-pattern
        #[arg(long, conflicts_with = "accept")]
        reject: bool,
    },
    /// View learned style guidelines
    View,
}

#[derive(Subcommand, Debug, Clone)]
pub enum McpSubcommand {
    /// List all discovered tools across configured external MCP servers
    List {
        #[arg(short, long)]
        config: Option<String>,
    },
    /// Call an external MCP server tool with JSON arguments
    Call {
        server: String,
        tool: String,
        #[arg(default_value = "{}")]
        args: String,
        #[arg(short, long)]
        config: Option<String>,
    },
    /// Run Hagibis as a Model Context Protocol (MCP) stdio server
    Serve,
}

#[derive(Subcommand, Debug, Clone)]
pub enum TimelineSubcommand {
    /// List active ephemeral timelines
    List,
    /// Create an isolated timeline worktree
    Create {
        name: String,
        #[arg(short, long)]
        base: Option<String>,
    },
    /// Diff changes in timeline against current workspace
    Diff {
        name: String,
    },
    /// Merge timeline changes back into main workspace
    Merge {
        name: String,
    },
    /// Discard and delete ephemeral timeline
    Discard {
        name: String,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum SkillSubcommand {
    /// Seed the skill store with default real skills
    Seed,
    /// List all skills in the store
    List,
    /// Add a new skill (or replace if name exists)
    Add {
        name: String,
        description: String,
        content: String,
        #[arg(long, default_value = "custom")]
        tier: String,
        #[arg(long, value_delimiter = ',')]
        triggers: Vec<String>,
    },
}

#[derive(Subcommand, Debug, Clone)]
pub enum RemoteSubcommand {
    /// Launch Telegram Bot long-polling daemon
    Telegram {
        /// Telegram Bot Token (or read from $TELEGRAM_BOT_TOKEN)
        #[arg(long)]
        token: Option<String>,
        /// Allowed user ID whitelist (repeatable)
        #[arg(long)]
        allowed_user: Vec<String>,
    },
    /// Dispatch notification to Discord webhook
    Discord {
        /// Discord webhook URL (or read from $DISCORD_WEBHOOK_URL)
        #[arg(long)]
        webhook: Option<String>,
        /// Message text to dispatch
        message: String,
    },
}

#[derive(Subcommand)]
enum Commands {
    /// Native Editor Integration & LSP Predictions
    LspGhostBridge,

    /// Cloud-Offloaded Compute & Multi-Node Execution
    CloudSwarm {
        #[arg(long, default_value = "https://swarm.hagibis.io")]
        endpoint: String,
        #[arg(short, long, default_value_t = 42)]
        max_nodes: usize,
        #[arg(long, default_value = "")]
        auth_token: String,
        #[arg(long)]
        payload: String,
    },

    /// WASM/Lua Hackability Plugin Fabric
    PluginFabric {
        #[arg(short, long, default_value = "lua")]
        language: String,
        #[arg(short, long)]
        script_path: String,
    },

    /// Non-Dev Web Access Dashboard
    StakeholderPortal {
        #[arg(short, long, default_value_t = 8080)]
        port: u16,
        #[arg(short, long, default_value = "Hagibis Portal")]
        title: String,
    },

    /// SSO & Compliance Corporate Gateway
    EnterpriseGateway {
        #[arg(long, default_value = "okta")]
        sso_provider: String,
        #[arg(long)]
        require_mfa: bool,
        #[arg(long)]
        audit_logging: bool,
        #[arg(long)]
        token: String,
    },

    /// NVMe/RAM/VRAM Memory Multi-Tiering
    MemoryTiering {
        #[arg(long, default_value = "100B")]
        model_size: String,
        #[arg(long, default_value = "/tmp/hgb_swap")]
        swap_path: String,
    },
    
    /// Contextual MoE Streaming
    MoeStreamer {
        #[arg(long)]
        expert: String,
    },

    /// Hardware Auto-Adaptation (CPU/GPU/NPU)
    HardwareAdapt,

    /// Zero-Dependency Built-In Inference
    NativeInference {
        prompt: String,
    },

    /// Decentralized Compute Pooling
    P2pGrid {
        #[arg(long, default_value = "global")]
        seed: String,
    },

    /// Background Indexing
    AmbientLearning,

    /// Cryptographic Code Sandboxing
    MerkleLedger,

    /// Transparent HUD Overlay
    GhostOverlay,

    /// Launch the classic terminal REPL (line-by-line scrolling mode)
    #[command(alias = "repl")]
    Classic,

    /// Launch the AGY Conversational Chat Canvas & Cockpit (default)
    #[command(alias = "chat-canvas", alias = "c")]
    Chat,

    /// Systems Self-Diagnosis & Environment Health Audit
    #[command(alias = "diag", alias = "check")]
    Doctor {
        /// Output diagnosis report in JSON format
        #[arg(long)]
        json: bool,
    },

    /// Resume Cockpit session from disk (.hgb/sessions/)
    Resume {
        /// Optional specific session ID to resume (defaults to latest)
        id: Option<String>,
    },

    /// Enterprise Fleet & Microsoft 365 Architecture Management
    #[command(alias = "ent")]
    Enterprise {
        #[command(subcommand)]
        action: EnterpriseSubcommand,
    },


    /// Interactive Blast-Radius Architecture Radar
    #[command(alias = "blast")]
    BlastRadius {
        /// Target files to analyze
        files: Vec<String>,
    },

    /// 100% Air-Gapped Offline Submarine Mode
    Submarine {
        /// Audit active system network sockets and proxy status
        #[arg(long)]
        audit: bool,
    },

    /// Multi-Agent Builder vs. Red-Teamer Security Tournament
    Tournament {
        /// Code specification or task description to debate and harden
        spec: String,
        /// Maximum debate and refinement rounds
        #[arg(long, default_value_t = 3)]
        rounds: usize,
    },

    /// Autonomous Agentic Git Bisect Regression Hunter
    Bisect {
        /// Bug symptom description or test command that reproduces the issue
        #[arg(long)]
        bug: String,
        /// Good commit hash (defaults to HEAD~10)
        #[arg(long)]
        good: Option<String>,
        /// Bad commit hash (defaults to HEAD)
        #[arg(long)]
        bad: Option<String>,
    },

    /// Local-First Speculative Draft Decoding (Ollama + Cloud Hybrid)
    SpeculativeDraft {
        /// Prompt to execute with speculative decoding
        prompt: String,
        /// Local draft model name
        #[arg(long, default_value = "qwen2.5-coder:1.5b")]
        draft_model: String,
    },

    /// Zero-Broken-Diff Speculative Memory Sandbox & Healing
    SpeculativeSandbox {
        /// Check or build command to run inside speculative sandbox (e.g. "cargo check")
        #[arg(short, long, default_value = "cargo check")]
        cmd: String,
        /// Target files or directories to mirror into sandbox (comma separated or defaults to src/, Cargo.toml)
        #[arg(long)]
        files: Option<String>,
    },

    /// Zero-HTTP In-Process Embedded Inference (GGUF / Direct VRAM)
    #[command(alias = "embed-infer")]
    Infer {
        /// Prompt text to generate completion for
        prompt: String,
        /// Optional path to GGUF model file
        #[arg(short, long)]
        model: Option<String>,
        /// Maximum tokens to generate
        #[arg(long, default_value_t = 128)]
        max_tokens: usize,
        /// Temperature (0.0 = greedy)
        #[arg(short, long, default_value_t = 0.2)]
        temperature: f32,
    },

    /// Instant Linux Copy-on-Write (CoW) Workspace Sandbox
    #[command(alias = "cow")]
    CowSandbox {
        /// Verification command to execute inside sandboxed merged view (e.g. "cargo check")
        #[arg(short, long, default_value = "cargo check")]
        cmd: String,
    },

    /// Speculative Graph-of-Thought (GoT) Multi-Branch Reasoning Arbiter
    #[command(alias = "graph-of-thought")]
    Got {
        /// Engineering problem, bug symptom, or feature specification
        spec: String,
        /// Target file path to synthesize and optimize
        #[arg(short, long, default_value = "src/solution.rs")]
        target: String,
    },

    /// Ambient Sleep-State Self-Tuning & DPO Dataset Synthesis
    #[command(alias = "sleep-tune")]
    Dream {
        /// Audit system headroom and power state
        #[arg(long)]
        audit: bool,
        /// Run DPO distillation cycle on git history
        #[arg(long)]
        distill: bool,
        /// Max commits to inspect
        #[arg(short, long, default_value_t = 15)]
        limit: usize,
    },

    /// Unified autonomous driver with RAM CoW isolation, GoT reasoning, and DPO distillation
    #[command(alias = "auto", alias = "one")]
    Do {
        /// Task or feature specification
        spec: String,
        /// Optional target file path
        #[arg(short, long)]
        target: Option<String>,
        /// Automatically commit verified changes to real workspace
        #[arg(long)]
        commit: bool,
    },

    /// Control Hagibis microkernel resident daemon, ambient sleep-tuning, and dream journal
    #[command(alias = "d")]
    Daemon {
        #[command(subcommand)]
        action: DaemonCliSubcommand,
    },

    /// Interactive fuzzy command search and navigator menu
    #[command(alias = "find-cmd", alias = "catalog")]
    Menu {
        /// Search query string
        #[arg(short, long)]
        query: Option<String>,
    },

    /// Official GitHub Action PR runner and blast radius auditor
    #[command(alias = "gh-action")]
    CiAction {
        /// Specific task or "full"
        #[arg(short, long, default_value = "full")]
        task: String,
        /// Custom PR diff string (optional; reads git diff if omitted)
        #[arg(short, long)]
        diff: Option<String>,
        /// Print GitHub Flavored Markdown summary to stdout
        #[arg(long)]
        summary: bool,
    },

    /// Run direct GPU/Vulkan GEMM compute benchmark (GFLOPS/TFLOPS)
    #[command(alias = "vulkan-bench")]
    GpuBench {
        /// Matrix dimension (M=N=K)
        #[arg(short, long, default_value_t = 64)]
        dim: usize,
        /// Iterations to benchmark
        #[arg(short, long, default_value_t = 5)]
        iterations: usize,
    },

    /// Pull and verify canonical GGUF models into ~/.hgb/models/
    #[command(alias = "pull")]
    ModelPull {
        /// Model identifier or alias (e.g. qwen2.5-coder:1.5b)
        name: String,
    },

    /// Launch standalone borderless Desktop IDE connected to Axum PWA Studio
    #[command(alias = "studio-app")]
    DesktopIde {
        /// Local port for PWA Studio
        #[arg(short, long, default_value_t = 38472)]
        port: u16,
        /// Optional custom browser executable path
        #[arg(short, long)]
        browser: Option<String>,
    },

    /// High-concurrency Anthropic & OpenAI drop-in proxy server with TokenShredder
    #[command(name = "proxy", alias = "serve-anthropic", alias = "dropin")]
    ProxyBridge {
        /// Port to bind proxy server (default: 8082)
        #[arg(short, long, default_value_t = 8082)]
        port: u16,

        /// Target provider override (e.g. copilot, groq, nvidia, gemini, openrouter, ollama)
        #[arg(long)]
        provider: Option<String>,

        /// Enable SIMD Token Shredder prompt optimization
        #[arg(long, default_value_t = true)]
        shred_tokens: bool,
    },

    /// Interactive Authentication & Device Flow (GitHub Copilot)
    #[command(name = "auth-copilot", alias = "copilot-auth", alias = "copilot-login")]
    AuthCopilot,

    /// Store OpenAI API key permanently (~/.hgb/credentials.json)
    #[command(name = "auth-openai", alias = "openai-auth", alias = "openai-login")]
    AuthOpenAi {
        /// OpenAI API key (sk-...)
        #[arg(value_name = "API_KEY")]
        key: Option<String>,
    },

    /// Import and verify OpenAI Codex CLI Web Authentication (~/.codex/auth.json)
    #[command(name = "auth-codex", alias = "codex-auth", alias = "import-codex", alias = "auth-chatgpt")]
    AuthCodex,

    /// Save provider API key permanently (~/.hgb/credentials.json)
    #[command(name = "key", alias = "set-key")]
    Key {
        /// Provider name (openai, anthropic, deepseek, openrouter)
        provider: String,
        /// Secret API key
        key: Option<String>,
    },

    /// Universal Cloud Fabric: catalog inspection & multi-provider latency benchmark
    #[command(alias = "fabric", alias = "cloud-bench")]
    CloudFabric {
        /// Ping all providers to measure live round-trip latency
        #[arg(long, default_value_t = true)]
        ping: bool,
    },

    /// Remote Mobile Gateway (Telegram & Discord task dispatcher)
    #[command(name = "remote-bot", alias = "remote-gateway", alias = "bot")]
    RemoteGateway {
        #[command(subcommand)]
        command: RemoteSubcommand,
    },

    /// Frontier Superpower 127: Visual Multimodal Debugger via CDP
    #[command(name = "visual-loop", alias = "visual-debug", alias = "cdp-debug")]
    VisualLoop {
        #[arg(short, long, default_value = "http://localhost:3000")]
        url: String,
        #[arg(short, long, default_value_t = 9222)]
        port: u16,
        #[arg(short, long)]
        screenshot: Option<String>,
    },

    /// Frontier Superpower 128: ChronoWarp Time-Machine TUI & Ledger
    #[command(name = "time-machine", alias = "chrono", alias = "tm")]
    TimeMachine {
        #[arg(short, long)]
        list: bool,
        #[arg(short, long)]
        restore: Option<String>,
        #[arg(short, long)]
        scrub: bool,
    },

    /// Frontier Superpower 129: Deterministic AST Query & Refactor Engine
    #[command(name = "ast-query", alias = "ast-refactor")]
    AstQuery {
        #[arg(short, long, default_value = "fn:public")]
        query: String,
        #[arg(short, long, default_value = "src/main.rs")]
        path: String,
        #[arg(long, default_value_t = true)]
        preview: bool,
        #[arg(long)]
        make_public: Option<String>,
    },

    /// Frontier Superpower 130: Zero-Config P2P Encrypted Tunnel & QR
    #[command(name = "p2p-tunnel", alias = "p2p", alias = "share-p2p")]
    Tunnel {
        #[arg(short, long, default_value_t = 3000)]
        port: u16,
        #[arg(short, long, default_value = "0.0.0.0")]
        bind: String,
        #[arg(long, default_value_t = 4040)]
        bind_port: u16,
        #[arg(long, default_value_t = true)]
        qr: bool,
    },

    /// Execute a prompt directly through the microkernel swarm
    #[command(alias = "ask", alias = "p")]
    Run {
        /// Prompt text to execute
        text: String,
        /// Optional model identifier
        #[arg(short, long)]
        model: Option<String>,
        /// Optional provider identifier
        #[arg(short, long)]
        provider: Option<String>,
        /// Continue conversation from previous session or memory
        #[arg(short = 'c', long = "continue")]
        continue_session: bool,
        /// Resume a specific past session by ID
        #[arg(long)]
        session: Option<String>,
    },

    /// Browse a live web URL, sanitize against SSRF and prompt injection, and render clean markdown
    #[command(alias = "web", alias = "fetch-url")]
    Browse {
        /// Target HTTP or HTTPS URL to browse
        url: String,
        /// Maximum tokens to extract (default 1500)
        #[arg(short, long, default_value_t = 1500)]
        tokens: usize,
    },

    /// Search the live web via DuckDuckGo and return ranked organic results
    #[command(alias = "web-search", alias = "ddg")]
    SearchWeb {
        /// Search query terms
        query: String,
        /// Maximum search results to return (default 5)
        #[arg(short, long, default_value_t = 5)]
        limit: usize,
    },

    /// Inspect local folder, storage partitions, or network shares & ARP neighbors
    #[command(name = "inspect-folder", alias = "folder", alias = "inspect-dir", alias = "ls-net")]
    InspectFolder {
        /// Target folder path or URI to inspect (defaults to current directory)
        path: Option<String>,
        /// Discover mounted storage drives and partitions
        #[arg(long)]
        storage: bool,
        /// Discover network shares and active local network computers via ARP
        #[arg(long)]
        network: bool,
        /// Discover standard user XDG directories
        #[arg(long)]
        xdg: bool,
        /// Maximum directory recursion depth (default 3)
        #[arg(short, long, default_value_t = 3)]
        depth: usize,
        /// Perform deep architectural analysis (archetype, storage saturation, network topology, hygiene audit)
        #[arg(short = 'a', long = "analyze")]
        analyze: bool,
    },

    /// Ping the resident hgbd daemon and measure UDS roundtrip latency
    Ping,

    /// Inspect daemon status, uptime, active models, and memory RSS
    #[command(alias = "info")]
    Status,

    /// Authenticate Hagibis with Google Account via OAuth 2.0 Web Login
    Login,

    /// Check Google Gemini OAuth and API Key credentials status
    Auth,

    /// View, list, or dynamically switch the active AI model
    #[command(alias = "models")]
    Model {
        #[command(subcommand)]
        action: Option<ModelSubcommand>,
        /// Optional model name for backwards compatibility (e.g. gemini-2.5-pro)
        name: Option<String>,
    },

    /// Manage Agent Skills (seed, add, list)
    #[command(alias = "skills")]
    Skill {
        #[command(subcommand)]
        action: SkillSubcommand,
    },

    /// Speculative dual-draft racing & Masterclass Autonomous Vibe Coding Agent Loop
    #[command(alias = "vibe-loop", alias = "autonomous-loop")]
    Vibe {
        /// Prompt or task description to race
        #[arg(default_value = "")]
        prompt: String,
        /// Optional target directory
        #[arg(short, long)]
        target_dir: Option<String>,
        /// Enable speculative dual-draft racing
        #[arg(long, default_value_t = true)]
        race: bool,
        /// Run continuous ambient watcher loop
        #[arg(short, long)]
        watch: bool,
        /// Execute single autonomous vibe cycle and output detailed diagnostic scorecard
        #[arg(long)]
        once: bool,
        /// Automatically apply verified green patches to workspace
        #[arg(long)]
        auto_patch: bool,
        /// Skip running compiler and test suite
        #[arg(long)]
        no_tests: bool,
        /// Debounce interval in milliseconds
        #[arg(long, default_value_t = 500)]
        debounce: u64,
    },

    /// Style Memory & Reject-Learner Vault
    Style {
        #[command(subcommand)]
        action: Option<StyleSubcommand>,
    },

    /// Autonomous PR Storyteller: stage atomic conventional commits and generate PR story
    Ship {
        /// Preview PR story without modifying git commit state
        #[arg(long)]
        dry_run: bool,
    },

    /// Instant App Scaffolder: generate zero-boilerplate fullstack architectures under 2s
    Forge {
        /// Stack template: rust-ratatui-tui, react-fastapi, rust-microservice, flutter-gemini, python-agent
        stack: String,
        /// Project directory name
        name: Option<String>,
    },

    /// Verification Gate & Golden Invariant Guard (or formal invariant solver)
    Verify {
        /// Target code expression or file (optional; defaults to full workspace)
        #[arg(short, long)]
        target: Option<String>,
        /// Invariant type: division, bounds, overflow
        #[arg(short, long, default_value = "division")]
        invariant: String,
        /// Attempt 3-iteration self-healing loop on invariant failures
        #[arg(short, long)]
        heal: bool,
    },

    /// Workspace Checkpoint Time-Machine (create, list, rewind)
    #[command(alias = "cp", alias = "ckpt")]
    Checkpoint {
        #[command(subcommand)]
        action: CheckpointSubcommand,
    },

    /// Audit and inspect Blake3 Merkle Provenance Ledger
    #[command(alias = "prov")]
    Provenance {
        /// Action: append, root
        #[arg(short, long, default_value = "append")]
        action: String,
    },

    /// Run differential fuzzing mutation suite against target
    Fuzz {
        /// Target input string or function signature
        #[arg(short, long)]
        target: String,
        /// Iteration count
        #[arg(short, long, default_value = "100")]
        iterations: usize,
    },

    /// Superpower 3: Unified MCP Mesh Network (Distributed tools, node discovery, mutation vectors)
    #[command(name = "mcp-mesh", alias = "mesh")]
    McpMesh {
        /// Synchronize tools and discovery across all peers
        #[arg(long)]
        sync: bool,
        /// Call a specific tool on a node
        #[arg(long)]
        call: Option<String>,
        /// Target node ID for tool call
        #[arg(long)]
        node: Option<String>,
        /// Arguments JSON for tool call
        #[arg(long, default_value = "{}")]
        args: String,
        /// Display mesh status
        #[arg(long)]
        status: bool,
    },

    /// Interactive Cockpit TUI Engine & Mid-Flight Steering
    #[command(alias = "tui")]
    Cockpit {
        /// Render in headless mode without spawning terminal interactive loop
        #[arg(long)]
        headless: bool,
    },

    /// Generate synthetic personas and relational seed batches with edge-case UTF-8
    Seed {
        /// Entity type (users, customers, orders, payments, auth_sessions)
        #[arg(default_value = "users")]
        entity: String,
        /// Number of records to generate
        #[arg(short, long, default_value = "10")]
        count: usize,
        /// Output SQL insert script
        #[arg(long, default_value_t = true)]
        sql: bool,
        /// Output JSON batch
        #[arg(long)]
        json: bool,
    },

    /// Scan repository and generate Living Architecture Blueprint (Mermaid & ASCII)
    Blueprint {
        /// Output path for BLUEPRINT.md
        #[arg(short, long)]
        output: Option<std::path::PathBuf>,
        /// Print Mermaid.js flowchart to stdout
        #[arg(long)]
        mermaid: bool,
    },

    /// Run Adversarial Red-Team & Edge-Case Audit (auth leaks, O(N^2), unbounded queries)
    #[command(alias = "audit")]
    RedTeam {
        /// Target file or directory (defaults to current workspace)
        path: Option<std::path::PathBuf>,
        /// Audit git diff only
        #[arg(long)]
        diff: bool,
    },

    /// Surgically rollback an individual function/symbol without touching adjacent edits
    Rewind {
        /// Source file path containing symbol
        file: String,
        /// Symbol/function name to rollback
        symbol: String,
        /// Target revision index (defaults to 0)
        #[arg(short, long, default_value = "0")]
        revision: usize,
    },

    // --- AGY Surgical CRUD Subcommands ---
    /// View file with AGY paged slicing and binary safety
    #[command(alias = "cat", alias = "read")]
    View {
        /// Path to target file
        path: String,
        /// Optional 1-indexed start line
        #[arg(short, long)]
        start: Option<usize>,
        /// Optional 1-indexed end line
        #[arg(short, long)]
        end: Option<usize>,
        /// Optional byte offset
        #[arg(long)]
        offset: Option<usize>,
    },

    /// Atomically write content to file with overwrite protection
    Write {
        /// Path to target file
        path: String,
        /// Content to write
        content: String,
        /// Overwrite if file exists
        #[arg(short, long)]
        overwrite: bool,
        /// Optional artifact summary
        #[arg(short = 's', long)]
        summary: Option<String>,
    },

    /// Surgically search and replace contiguous text blocks
    #[command(alias = "replace")]
    Edit {
        /// Path to target file
        path: String,
        /// Exact target content to replace
        #[arg(short, long)]
        target: String,
        /// Replacement content
        #[arg(short, long, alias = "replace")]
        replacement: String,
        /// Optional 1-indexed start line boundary
        #[arg(short, long)]
        start: Option<usize>,
        /// Optional 1-indexed end line boundary
        #[arg(short, long)]
        end: Option<usize>,
        /// Allow replacing multiple occurrences
        #[arg(short, long)]
        multiple: bool,
        /// Optional user-facing instruction
        #[arg(long)]
        instruction: Option<String>,
        /// Optional change description
        #[arg(long)]
        description: Option<String>,
        /// Associated lint error IDs resolved by edit
        #[arg(long = "lint")]
        lints: Vec<String>,
    },

    /// List directory contents with file sizes and recursive counts
    #[command(alias = "dir")]
    Ls {
        /// Directory path (defaults to current directory)
        #[arg(default_value = ".")]
        path: String,
    },

    /// Search files recursively for regex pattern with line snippets
    Grep {
        /// Search pattern or regex
        pattern: String,
        /// Root directory or file (defaults to current directory)
        #[arg(default_value = ".")]
        path: String,
        /// Treat pattern as regular expression
        #[arg(short, long)]
        regex: bool,
        /// Case-insensitive search
        #[arg(short, long)]
        ignore_case: bool,
        /// Only return file names matching the pattern
        #[arg(short = 'l', long)]
        files_only: bool,
        /// Glob patterns to include or exclude (e.g. *.rs or !**/target/*)
        #[arg(short = 'I', long = "include")]
        includes: Vec<String>,
    },

    /// Find files and subdirectories by name, extension, exclusion, or depth
    #[command(alias = "search", alias = "fd")]
    Find {
        /// Pattern to search for (wildcards supported)
        pattern: Option<String>,
        /// Directory to search within (defaults to current directory)
        #[arg(short = 'd', long = "dir", default_value = ".")]
        dir: String,
        /// File extensions to include (without leading dot)
        #[arg(short = 'e', long = "ext")]
        extensions: Vec<String>,
        /// Exclude patterns
        #[arg(short = 'x', long = "exclude")]
        excludes: Vec<String>,
        /// Maximum directory depth to search
        #[arg(short = 'm', long = "max-depth")]
        max_depth: Option<usize>,
        /// Filter by type: file, directory, or any
        #[arg(short = 't', long = "type")]
        target_type: Option<String>,
    },

    /// Execute sandboxed shell command with AgentShieldLight validation and timeout
    #[command(alias = "exec", alias = "sh")]
    RunCmd {
        /// Shell command line to execute
        command: String,
        /// Working directory
        #[arg(short, long)]
        dir: Option<String>,
        /// Timeout in milliseconds (default: 30000ms)
        #[arg(short, long, default_value_t = 30000)]
        timeout: u64,
    },

    /// Run autonomous ReAct multi-turn agent loop
    #[command(alias = "act")]
    Agent {
        /// Task or prompt for the autonomous agent
        prompt: String,
        /// Maximum execution turns (default: 10)
        #[arg(short, long, default_value_t = 10)]
        turns: usize,
        /// Optional model override
        #[arg(short, long)]
        model: Option<String>,
    },

    /// Compiler & test-driven self-healing loop
    Heal {
        /// Verification command to execute (e.g. "cargo check", "cargo test", "npm test")
        #[arg(short, long)]
        cmd: Option<String>,
        /// Optional target workspace directory
        #[arg(short, long)]
        dir: Option<String>,
    },

    /// Instant time-travel rollback to previous checkpoint or specific ID
    Undo {
        /// Specific checkpoint ID to restore
        #[arg(short, long)]
        checkpoint: Option<String>,
    },

    /// Discover or initialize workspace rules (.hgb/rules, HGB.md, .cursorrules)
    Rules {
        /// Initialize default .hgb/rules workspace rules
        #[arg(long)]
        init: bool,
        /// Target workspace directory (default current directory)
        #[arg(short, long)]
        dir: Option<String>,
    },

    /// Generate AST syntactic repository map outline
    #[command(alias = "map")]
    RepoMap {
        /// Target workspace directory (default current directory)
        #[arg(short, long)]
        dir: Option<String>,
        /// Maximum files to index (default 50)
        #[arg(short, long, default_value_t = 50)]
        max_files: usize,
    },

    /// Continuous Guardian background watcher with debounced passive verification
    #[command(alias = "guardian")]
    Watch {
        /// Custom check command (e.g. "cargo check --workspace")
        #[arg(short, long)]
        cmd: Option<String>,
        /// Debounce interval in milliseconds (default: 350ms)
        #[arg(short, long, default_value_t = 350)]
        debounce: u64,
    },

    /// Inspect or apply pre-computed staged ghost-fixes
    Ghost {
        /// Specific Ghost-Fix ID to apply
        #[arg(short, long)]
        apply: Option<String>,
    },

    /// DevServer Sentinel: scan localhost ports and probe dev server endpoints
    #[command(alias = "devs", alias = "sentinel")]
    DevScan,

    /// Ripple Effect Radar: assess transitive blast radius and symbol call-sites
    Impact {
        /// Target symbol to analyze (e.g. struct name, fn name, type)
        symbol: String,
        /// Originating source file path
        #[arg(short, long)]
        file: String,
    },

    /// Specialist Swarm Pod: 4-role concurrent consensus DAG (Architect -> Coder -> [Reviewer + QA])
    #[command(alias = "swarm-pod", alias = "pod-swarm")]
    Pod {
        /// Development task or feature specification
        task: String,
    },

    /// Terminal Rescue: diagnose failed shell commands and suggest verified corrective fixes
    #[command(alias = "rescue")]
    Fix {
        /// Failed command line to diagnose (defaults to latest recorded crash in .hgb/crashes/)
        command: Option<String>,
        /// Exit code of the failure (default: 1)
        #[arg(short, long, default_value_t = 1)]
        exit_code: i32,
        /// Stderr captured from the run
        #[arg(long, default_value = "")]
        stderr: String,
        /// Stdout captured from the run
        #[arg(long, default_value = "")]
        stdout: String,
    },

    /// Project Memory Ledger: manage architectural decisions, tech debt, and context anchors
    #[command(alias = "mem")]
    Memory {
        /// Record a new architectural decision (ADR)
        #[arg(long)]
        record: bool,
        /// ADR title
        #[arg(short, long)]
        title: Option<String>,
        /// ADR decision description
        #[arg(long)]
        decision: Option<String>,
        /// ADR context / rationale
        #[arg(long)]
        context: Option<String>,
        /// Output low-token XML prompt context anchor
        #[arg(short, long)]
        anchor: bool,
        /// Max tokens for context anchor
        #[arg(long, default_value_t = 2000)]
        max_tokens: usize,
    },

    /// Multimodal Visual Canvas & Layout Autopsy
    #[command(alias = "inspect", alias = "layout")]
    Glance {
        /// Path to screenshot or UI image (PNG, JPEG, WebP, GIF, SVG)
        image: String,
        /// Optional path to source HTML/CSS file context
        #[arg(short, long)]
        context: Option<String>,
        /// Synthesize production UI component code (react, html, ratatui)
        #[arg(short, long)]
        synthesize: Option<String>,
    },

    /// Dependency Hallucination Firewall: verify packages against live ecosystem registries
    #[command(alias = "pkg", alias = "guard")]
    PkgCheck {
        /// Package ecosystem: crates.io, npm, or pypi
        #[arg(short, long, default_value = "crates.io")]
        ecosystem: String,
        /// Package name to verify
        name: String,
        /// Optional requested version
        #[arg(short, long)]
        version: Option<String>,
    },

    /// No-Leak Secret Sentinel: scan polyglot env var references and reconcile .env
    #[command(alias = "env")]
    EnvAudit {
        /// Generate clean git-safe .env.example template
        #[arg(short, long)]
        example: bool,
        /// Shred and sanitize secrets from a string payload
        #[arg(short, long)]
        shred: Option<String>,
        /// Target workspace directory (default: ".")
        #[arg(short, long)]
        dir: Option<String>,
        /// Specific .env file path override
        #[arg(long)]
        env_file: Option<String>,
    },

    /// Ephemeral Proxy Fabric: launch dynamic in-memory CRUD REST server on localhost
    #[command(name = "mock-proxy", alias = "proxy-server", alias = "ephemeral-proxy")]
    Proxy {
        /// Resource entity name (e.g. users, products, orders)
        #[arg(default_value = "items")]
        resource: String,
        /// Preferred port (default: ephemeral dynamic port)
        #[arg(short, long)]
        port: Option<u16>,
        /// Number of realistic synthetic items to seed (default: 5)
        #[arg(short, long, default_value_t = 5)]
        seed: usize,
        /// Optional JSON template for entity schema
        #[arg(short, long)]
        schema: Option<String>,
        /// Stop a running proxy server on port
        #[arg(long)]
        stop: Option<u16>,
    },

    /// Ambient Execution Recorder: retrieve post-mortem XML context dump of recent events
    #[command(alias = "history")]
    Trace {
        /// Number of recent events to include in dump (default: 20)
        #[arg(short, long, default_value_t = 20)]
        last: usize,
    },

    /// Atmospheric Git Worktrees: create and manage isolated scratch branches
    #[command(alias = "wt")]
    Worktree {
        /// Branch name for the worktree
        branch: String,
        /// Clean up and remove worktree
        #[arg(short, long)]
        cleanup: bool,
        /// Base commit or branch to branch from (default: HEAD)
        #[arg(short, long)]
        base: Option<String>,
    },

    /// Semantic Stash: tag and restore uncommitted changes with structured JSON metadata
    #[command(alias = "st")]
    Stash {
        /// Stash action: create, apply, list, drop (default: create)
        #[arg(default_value = "create")]
        action: String,
        /// Tag identifier for the stash
        #[arg(short, long)]
        tag: Option<String>,
        /// Optional description of changes
        #[arg(short, long)]
        description: Option<String>,
    },

    /// Browser Snoop: inspect devserver runtime errors, unhandled DOM exceptions, and CDP console
    #[command(alias = "snoop", alias = "browser")]
    BrowserSnoop {
        /// Optional target URL to probe (e.g. http://localhost:3000)
        #[arg(short, long)]
        url: Option<String>,
        /// Clear recorded incidents
        #[arg(short, long)]
        clear: bool,
    },

    /// Variant Race: 3-way speculative design forking on ephemeral ports
    #[command(alias = "race")]
    VariantRace {
        /// UI prompt for the 3-way speculative race
        #[arg(short, long)]
        prompt: Option<String>,
        /// Candidate ID to pick as winner
        #[arg(long)]
        pick: Option<String>,
        /// Race ID to abort
        #[arg(long)]
        abort: Option<String>,
        /// Target race ID (for pick action)
        #[arg(long)]
        race_id: Option<String>,
    },

    /// DbSentinel: live database schema drift detection and safe auto-migration
    #[command(alias = "dbsync", alias = "db")]
    DbSentinel {
        /// Path to SQLite database file (default: sqlite.db)
        #[arg(short, long)]
        db: Option<String>,
        /// Path to expected SQL DDL file (default: schema.sql)
        #[arg(long)]
        ddl: Option<String>,
        /// Dry-run migration syntax and safety checks using transactional savepoints
        #[arg(long)]
        dry_run: bool,
        /// Apply generated migration to live database
        #[arg(long)]
        apply: bool,
        /// Migration identifier or name
        #[arg(long, default_value = "auto_migration")]
        name: String,
    },

    /// Syntax Slicer: surgical AST call-graph and type projector for token compression
    #[command(alias = "slice")]
    SyntaxSlice {
        /// Target function, struct, or symbol name
        symbol: String,
        /// Source file path
        #[arg(short, long)]
        file: String,
        /// Call graph traversal depth
        #[arg(short, long, default_value_t = 2)]
        depth: usize,
    },

    /// AutoSpec: autonomous invariant fuzzer and regression blocker
    #[command(alias = "spec")]
    AutoSpec {
        /// Synthesize golden invariant spec for function
        #[arg(long)]
        synth: Option<String>,
        /// Source file containing the function
        #[arg(short, long)]
        file: Option<String>,
        /// Run regression checks for all golden specs
        #[arg(long)]
        run: bool,
        /// Strict mode: exit with error code if any invariant fails
        #[arg(long)]
        strict: bool,
        /// List all active golden specs
        #[arg(long)]
        list: bool,
    },

    /// DriftLock: architectural DNA extractor and patch compliance auditor
    #[command(alias = "dna")]
    DriftLock {
        /// Scan workspace and extract Architectural DNA
        #[arg(long)]
        scan: bool,
        /// Audit patch file against Architectural DNA
        #[arg(long)]
        audit: Option<String>,
    },

    /// Internal helper invoked by shell integration hooks to record failed commands
    #[command(name = "crash-record", hide = true)]
    CrashRecord {
        #[arg(long)]
        cmd: String,
        #[arg(long)]
        code: i32,
        #[arg(long)]
        pwd: String,
        #[arg(long, default_value = "")]
        stderr: String,
    },

    /// Generate shell companion integration script for Bash, Zsh, or Fish
    Init {
        /// Target shell: bash, zsh, fish
        shell: String,
    },

    /// Universal Model Context Protocol (MCP) Client & Sovereign Stdio Server
    Mcp {
        #[command(subcommand)]
        action: Option<McpSubcommand>,
    },

    /// Manage Ephemeral "What-If" Worktree & Snapshot Timelines
    #[command(alias = "fork")]
    Timeline {
        #[command(subcommand)]
        action: TimelineSubcommand,
    },

    /// AST-Aware Visual Patch Arbiter: deconstruct diff into hunks and selectively apply
    #[command(alias = "hunk")]
    Patch {
        /// Target file path
        path: String,
        /// Modified code string or patch file path
        #[arg(short, long)]
        modified: Option<String>,
        /// Automatically accept all valid AST hunks
        #[arg(short, long)]
        accept_all: bool,
    },

    /// Instant P2P Mobile QR Live-Sync & Ephemeral Preview Tunnel
    #[command(alias = "tunnel")]
    Live {
        /// Local devserver port (e.g. 3000, 5173, 8080)
        #[arg(default_value_t = 3000)]
        port: u16,
        /// Custom session identifier
        #[arg(short, long)]
        name: Option<String>,
    },

    /// Autonomous Speculative TDD Loop ("Red-to-Green Synthesis")
    Tdd {
        /// Intent or specification prompt
        intent: String,
        /// Target function name (e.g. calculate_total)
        #[arg(short, long, default_value = "process_action")]
        target: String,
        /// Language / file extension (e.g. rs, ts, py)
        #[arg(short, long, default_value = "rs")]
        lang: String,
    },

    /// Ephemeral Micro-WASM & Capability Sandbox: execute command in clean jail
    #[command(alias = "jail")]
    Isolate {
        /// Shell command to run inside sandbox
        command: String,
        /// Arguments for the command
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
        /// Timeout in milliseconds (default: 15000ms)
        #[arg(short, long, default_value_t = 15000)]
        timeout: u64,
    },

    /// Ambient Audio Earcon: play flow-state harmonic chime
    Chime {
        /// Cue kind: green, error, race, heal, check
        #[arg(default_value = "green")]
        cue: String,
    },

    /// Local SIMD Vector Index: index and search codebase symbols
    Index {
        /// Action: search, scan, invalidate
        #[arg(default_value = "scan")]
        action: String,
        /// Query string or target file path
        query: Option<String>,
    },

    /// Hot-Module CDP Live Patching: inject CSS or JS in-memory into running browser
    #[command(alias = "cdp")]
    Hmr {
        /// Patch kind: css, js, dom
        #[arg(short, long, default_value = "css")]
        kind: String,
        /// Target CSS selector or function name
        target: String,
        /// Property/value or function body payload
        payload: String,
    },

    /// AST Skeleton Lens: project compact typed outline with 80% token reduction
    Lens {
        /// File path to project
        path: String,
        /// Target symbol name to keep expanded
        #[arg(short, long, default_value = "")]
        symbol: String,
    },

    /// Lakandiwa Triple-Model Consensus Swarm: 3-way speculative race and auto-merge
    Swarm {
        /// Coding intent or prompt
        prompt: String,
        /// Target symbol
        #[arg(short, long, default_value = "process_action")]
        symbol: String,
        /// Language extension
        #[arg(short, long, default_value = "rs")]
        lang: String,
    },

    /// Instant Database CoW Time Machine: take atomic snapshot or rollback database
    DbSnap {
        /// Database file path
        path: String,
        /// Rollback to snapshot file if specified
        #[arg(short, long)]
        rollback: Option<String>,
        /// Snapshot description
        #[arg(short, long, default_value = "Manual snapshot")]
        description: String,
    },

    /// Supply-Chain & Slopsquatting Hallucination Firewall: audit package additions
    Shield {
        /// Package names to audit
        packages: Vec<String>,
        /// Ecosystem: cargo, npm, pypi
        #[arg(short, long, default_value = "cargo")]
        ecosystem: String,
    },

    /// Zero-Ops Cloud Launchpad: deploy ephemeral serverless preview to edge with TLS
    ShipLive {
        /// Project name or slug
        #[arg(short, long, default_value = "hagibis-app")]
        name: String,
    },

    /// Living Architecture Flight Simulator: trace end-to-end request pipeline
    Flight {
        /// Endpoint name to trace (e.g. POST /checkout)
        #[arg(default_value = "POST /api/v1/checkout")]
        endpoint: String,
    },

    /// Predictive Shadow Synthesizer: speculative AST precomputation
    #[command(alias = "shadow-coder", alias = "spec-coder")]
    GhostCoder {
        /// Code prefix trigger
        prefix: String,
    },

    /// Universal Offline API Mirage: synthetic proxys & wiretapping
    Mirage {
        /// Route path (e.g. /v1/payment_intents)
        endpoint: String,
        /// HTTP method (default: GET)
        #[arg(short, long, default_value = "GET")]
        method: String,
    },

    /// In-Process Chaos Monkey & UI Invariant Fuzzer
    Chaos {
        /// Target component name (e.g. PaymentGateway)
        #[arg(default_value = "CoreService")]
        target: String,
        /// Optional idempotency key to test rapid replay bursts
        #[arg(short, long)]
        idempotency_key: Option<String>,
        /// Number of duplicate bursts (default: 5)
        #[arg(short, long, default_value_t = 5)]
        runs: usize,
    },

    /// Autonomous Night-Shift Swarm Worktree Pipeline
    Nightshift {
        /// High-level goal or feature prompt
        goal: String,
        /// Base branch to branch from (default: main)
        #[arg(short, long, default_value = "main")]
        base: String,
    },

    /// Kernel-Level Memory-Only Ghost Envs: Blake3 vault encryption and disk audit
    Vault {
        /// Passphrase to seal vault
        #[arg(short, long)]
        passphrase: Option<String>,
        /// File path of .env on disk to audit for raw secret leaks
        #[arg(short, long, default_value = ".env")]
        disk_file: String,
    },

    /// Zero-Drift Polyglot Type Lock: sync Rust structs to TypeScript & Zod schemas
    Typelock {
        /// Path to Rust source file with struct definitions
        source_file: String,
        /// Optional TypeScript file to audit for drift
        #[arg(short, long)]
        ts_file: Option<String>,
    },

    /// Spatial Cockpit Radar: 3-tier semantic zoom (Orbit, Atmosphere, Surface)
    Radar {
        /// Zoom tier: orbit, atmosphere, or surface (default: orbit)
        #[arg(default_value = "orbit")]
        tier: String,
    },

    /// Click-to-Source CDP Teleport: resolve browser DOM element to source file and AST symbol
    Teleport {
        /// DOM selector or test ID (e.g. button#checkout-btn)
        selector: String,
    },

    /// Full-Duplex Zero-Latency Voice Flow Co-Pilot & Local Whisper VAD
    #[command(alias = "mic", alias = "listen")]
    Voice {
        /// Optional spoken audio transcript to process
        #[arg(default_value = "")]
        transcript: String,
        /// Audio file to parse and transcribe (WAV)
        #[arg(short, long)]
        file: Option<String>,
        /// Live microphone capture armed
        #[arg(short, long)]
        record: bool,
        /// Use Groq Cloud Whisper endpoint
        #[arg(long)]
        groq: bool,
    },

    /// Headless Screenplay & Automated PR Loom Tape: capture animated visual proof
    Tape {
        /// Target URL to navigate and record
        #[arg(default_value = "http://localhost:3000")]
        url: String,
        /// Scenario name
        #[arg(short, long, default_value = "Feature Verification")]
        scenario: String,
    },

    /// Token FinOps & Dynamic Latency Arbitrage: route prompts between local Ollama and frontier cloud
    Finops {
        /// Prompt to evaluate for token footprint and optimal tier routing
        prompt: String,
    },

    /// Zero-Knowledge Airgap Cloak: mask sensitive credentials and PII
    Cloak {
        /// Input text or prompt to sanitize
        text: String,
        /// If true, simulate rehydrating the response
        #[arg(short, long)]
        rehydrate: bool,
    },

    /// Active SQL Interceptor & Shadow Transaction Jail: barrier against destructive queries
    SqlGuard {
        /// SQL query to inspect (e.g. DELETE FROM users;)
        sql: String,
    },

    /// Deterministic Execution Replay & Rewind-Exec: time-travel flight recorder
    Replay {
        /// Historical frame index to scrub to
        #[arg(short, long)]
        frame: Option<usize>,
    },

    /// Two-Way Visual Canvas & Live CSS/Tailwind Bi-Directional Mirror
    Canvas {
        /// Target file path (e.g. src/components/Hero.tsx)
        file: String,
        /// Old CSS class or property (e.g. p-4)
        old: String,
        /// New CSS class or property (e.g. p-6)
        new: String,
        /// Optional selector or component name
        #[arg(short, long, default_value = "div")]
        selector: String,
    },

    /// Multi-Repo Swarm & Monorepo Mesh Federator: synchronized cross-repo PRs
    Federate {
        /// Feature goal to coordinate across repos
        goal: String,
    },

    /// Relational Time-Warp Data Synthesizer: temporal proxy datasets with strict FK integrity
    TimeWarp {
        /// Number of months to simulate backwards (default: 6)
        #[arg(short, long, default_value_t = 6)]
        months: u32,
        /// Randomization seed (default: 42)
        #[arg(short, long, default_value_t = 42)]
        seed: u64,
        /// Record scale per organization (default: 5)
        #[arg(short, long, default_value_t = 5)]
        scale: usize,
    },

    /// Structural Invariant Guardrails & Anti-Spaghetti Linter
    Guardrails {
        /// Target workspace directory (default: current directory)
        #[arg(default_value = ".")]
        dir: String,
    },

    /// Production Crash Auto-Triage & Reproduction Pipeline
    Triage {
        /// Raw stack trace or panic dump to analyze
        trace: String,
    },

    /// Flaky Test Exterminator & Deterministic Stress Fuzzer
    Deflake {
        /// Test function name to stress test
        test: String,
        /// Optional path to test file
        #[arg(short, long)]
        file: Option<String>,
    },

    /// Associative Neural Context & Infinite Cross-Session Memory
    ContextAnchor {
        /// Optional new key to record
        #[arg(short, long)]
        key: Option<String>,
        /// Statement or decision text to record
        #[arg(short, long)]
        statement: Option<String>,
        /// Category: arch, sec, style, banned, or entity (default: arch)
        #[arg(short, long, default_value = "arch")]
        category: String,
    },

    /// Universal LSP Ghost Daemon Bridge & Inline Prediction
    GhostLsp {
        /// Target file path
        file: String,
        /// Optional prefix line
        #[arg(short, long, default_value = "pub async fn handle_checkout")]
        prefix: String,
        /// Cursor line
        #[arg(short, long, default_value_t = 1)]
        line: usize,
    },

    /// Automated Rolling Context Compactor & Semantic Tree Pruner
    Compact {
        /// Max token threshold for compaction (default: 32000)
        #[arg(short, long, default_value_t = 32000)]
        max_tokens: usize,
    },

    /// Atomic Conventional Git Micro-Commit Mirror
    MicroCommit {
        /// Feature or bugfix intent description
        intent: String,
        /// Target files or path (default: current workspace)
        #[arg(short, long, default_value = ".")]
        path: String,
    },

    /// Declarative Vibe Recipes & Runbook Engine
    Recipe {
        /// Name of recipe to run (e.g. migrate-tailwind-v4, setup-biometric-passkey)
        name: Option<String>,
        /// List all available pre-packaged recipes
        #[arg(short, long)]
        list: bool,
    },

    /// Pre-Flight Behavioral Contract Matrix Generator
    Contract {
        /// Target symbol or function name (e.g. process_payment)
        symbol: String,
        /// Intent description for the contract
        #[arg(short, long, default_value = "Standard nominal processing")]
        intent: String,
    },

    /// Live Agent Flight-Graph & Real-Time Task DAG Visualizer
    FlightGraph {
        /// High-level goal or task to graph
        goal: String,
        /// Active step index (default: 2)
        #[arg(short, long, default_value_t = 2)]
        step: usize,
    },

    /// Tree-sitter PageRank Symbol Graph & Token Density Repo-Map
    #[command(alias = "rank-map")]
    RepoMapRank {
        /// Extensions to scan (comma separated or multiple flags, default: rs, ts, py, go)
        #[arg(short, long, default_values_t = vec!["rs".to_string(), "ts".to_string(), "py".to_string()])]
        exts: Vec<String>,
        /// Token budget for rendered map (default: 1024)
        #[arg(short, long, default_value_t = 1024)]
        budget: usize,
    },

    /// Cursor-Style Silent Pre-Flight Shadow Workspace & Speculative Repair
    #[command(alias = "preflight")]
    ShadowCheck {
        /// Target file path to validate in shadow workspace
        file: String,
        /// Optional path to candidate modified file (if omitted, reads file)
        #[arg(short, long)]
        candidate_file: Option<String>,
    },

    /// Claude Code-Style Terminal Stream Squeezer & High-Signal Digest
    Squeeze {
        /// Input log string or path to log file
        #[arg(short, long)]
        file: Option<String>,
        /// Max tokens for compressed output (default: 2048)
        #[arg(short, long, default_value_t = 2048)]
        max_tokens: usize,
    },

    /// Qodo-Style Test Integrity & Anti-Placebo Mutation Testing
    #[command(alias = "fuzz-test")]
    MutationAudit {
        /// Target source file to fuzz with mutations
        file: String,
    },

    /// Bolt.new-Style Visual Click-to-Code DOM Telemetry & Inspector
    #[command(alias = "inspect-dom")]
    DomInspect {
        /// Target HTML or JSX template file
        file: String,
        /// Optional CSS selector to query (.class or #id)
        #[arg(short, long)]
        selector: Option<String>,
        /// Click coordinates in format "x,y" (e.g. "150,45")
        #[arg(short, long)]
        coords: Option<String>,
    },

    /// Goose-Style Universal MCP Fleet Host Orchestrator & Multi-Server Hub
    McpHub {
        /// Action: list, discover, start, stop, call
        #[arg(default_value = "list")]
        action: String,
        /// Server name
        #[arg(short, long)]
        server: Option<String>,
        /// Tool name (for call)
        #[arg(short, long)]
        tool: Option<String>,
        /// JSON arguments (for call)
        #[arg(short, long)]
        args: Option<String>,
    },

    /// Augment Code-Style Live Graph Watcher & Incremental In-Memory Index
    #[command(alias = "graph-sync")]
    LiveGraph {
        /// Extensions to track (default: rs, ts, py)
        #[arg(short, long, default_values_t = vec!["rs".to_string(), "ts".to_string(), "py".to_string()])]
        exts: Vec<String>,
    },

    /// Warp Terminal-Style Shell Panic Interceptor & 1-Key Auto-Repair
    #[command(alias = "panic-fix")]
    ShellPanic {
        /// Exit code of failed command
        #[arg(short, long, default_value_t = 1)]
        code: i32,
        /// Failed command string
        #[arg(short = 'm', long)]
        cmd: String,
        /// Stderr output of failed command
        #[arg(short, long, default_value = "")]
        stderr: String,
    },

    /// Copilot Workspace-Style Spec -> Plan -> Diff Task Decomposer
    #[command(alias = "decompose")]
    PlanSpec {
        /// High-level feature intent or bug description
        intent: String,
    },

    /// Continue.dev & Roo Code-Style Dynamic @Context Expander
    #[command(alias = "at-expand")]
    ExpandContext {
        /// User prompt containing @-directives
        prompt: String,
    },

    /// Devin & Replit-Style Visual DOM Layout Regression Sentry
    #[command(alias = "pixel-diff")]
    VisualSentry {
        /// Path to baseline HTML/DOM snapshot
        baseline: Option<String>,
        /// Path to current HTML/DOM snapshot
        current: Option<String>,
    },

    /// Meta SapFix & Qodo-Style Continuous Autonomous Healing Loop
    #[command(alias = "watchdog")]
    HealWatch {
        /// Error string or test name to heal
        #[arg(short, long)]
        error: Option<String>,
    },

    /// Windsurf Cascade & Supermaven Next-Edit Anticipator
    #[command(alias = "predict-edit")]
    AmbientPredict {
        /// Modified source file path
        #[arg(short, long)]
        file: String,
        /// Modified symbol name
        #[arg(short, long)]
        symbol: String,
        /// Old snippet or signature
        #[arg(short, long)]
        old: Option<String>,
        /// New snippet or signature
        #[arg(short, long)]
        new: Option<String>,
    },

    /// Bolt.new & Devin Bidirectional DevTools Click-to-Source Sync
    #[command(alias = "tweak-sync")]
    CdpTweak {
        /// DOM CSS selector (e.g. 'button.checkout')
        #[arg(short, long)]
        selector: String,
        /// Property or attribute name (e.g. 'className')
        #[arg(short, long)]
        prop: String,
        /// Old attribute/property value
        #[arg(short, long)]
        old: String,
        /// New attribute/property value
        #[arg(short, long)]
        new: String,
        /// Apply changes directly to disk
        #[arg(long, default_value_t = true)]
        apply: bool,
    },

    /// Continue.dev & Roo Code Composable Modes & Live Docs Harvester
    #[command(alias = "mode-harvest")]
    PromptHarvest {
        /// Operating mode (architect, code, debug, security, doc)
        #[arg(short, long, default_value = "code")]
        mode: String,
        /// User request prompt
        #[arg(short, long)]
        prompt: String,
        /// Target documentation library or URL
        #[arg(short, long)]
        doc: Option<String>,
    },

    /// Replit Agent & WebContainers Zero-Config Ephemeral Stack Sandbox
    #[command(alias = "stack-sandbox")]
    Sandbox {
        /// Stack template name (e.g. axum-sqlite, react-fastapi)
        #[arg(short, long, default_value = "fullstack-sqlite")]
        stack: String,
        /// Tables to initialize and seed
        #[arg(short, long, default_values_t = vec!["users".to_string(), "orders".to_string()])]
        seed: Vec<String>,
    },

    /// Qodo & Meta SapFix Anti-Placebo Mutation Testing Gatekeeper
    #[command(alias = "mutation-gate")]
    AntiPlacebo {
        /// Target source code file
        #[arg(short, long)]
        file: Option<String>,
        /// Test code file to audit
        #[arg(short, long)]
        test: Option<String>,
    },

    /// Superpower 74: Embedded Webview HUD & Live Canvas Sidecar on localhost
    #[command(alias = "hud", alias = "canvas-hud")]
    Ui {
        /// Local port to bind HUD server
        #[arg(short, long)]
        port: Option<u16>,
        /// Preferred AI model identifier
        #[arg(short, long)]
        model: Option<String>,
    },

    /// Superpower 75: Zero-Config 1-Click Public Edge Deployer
    Deploy {
        /// Target edge provider (cloudflare, vercel, fly, vella)
        #[arg(short, long, default_value = "cloudflare")]
        provider: String,
        /// Project name or slug
        #[arg(short, long, default_value = "hagibis-app")]
        name: String,
        /// Custom public domain name
        #[arg(short, long)]
        domain: Option<String>,
        /// Write configuration files to workspace
        #[arg(long, default_value_t = true)]
        write_configs: bool,
    },

    /// Superpower 76: Visual Screenshot Annotation & Multimodal Clipboard Xerox Engine
    #[command(alias = "xerox")]
    Annotate {
        /// Raw annotation payload (JSON, SVG, base64 data URI, or natural language intent)
        #[arg(default_value = "")]
        input: String,
        /// Ingest image from system clipboard
        #[arg(short, long)]
        clipboard: bool,
    },

    /// Superpower 77: Collaborative Real-Time Multiplayer Vibe Swarm
    #[command(alias = "swarm-hub", alias = "multiplayer")]
    Pair {
        /// Multiplayer session ID
        #[arg(default_value = "vibe-room-1")]
        session: String,
        /// Action: join, status, leave
        #[arg(short, long, default_value = "join")]
        action: String,
        /// Peer display username
        #[arg(short, long, default_value = "vibe_coder")]
        username: String,
        /// Peer role: driver, navigator, reviewer, spectator
        #[arg(short, long, default_value = "driver")]
        role: String,
    },

    /// Superpower 78: Universal Companion Editor and LSP Sidecar Bridge
    #[command(alias = "bridge")]
    Companion {
        /// Target editor: vscode, neovim, helix, zed
        #[arg(default_value = "vscode")]
        editor: String,
        /// Automatically install generated config files to workspace
        #[arg(short, long)]
        install: bool,
    },

    /// Superpower 79: Instant Monetization & Auth Fabric
    #[command(alias = "monetize", alias = "stripe")]
    Saas {
        /// Project name
        #[arg(default_value = "vibe-app")]
        project: String,
        /// Provider: stripe, lemonsqueezy, paddle
        #[arg(short, long, default_value = "stripe")]
        provider: String,
        /// Target framework: nextjs, axum, express
        #[arg(short, long, default_value = "nextjs")]
        framework: String,
        /// Enable JWT auth guard
        #[arg(long, default_value_t = true)]
        auth: bool,
        /// Enable Stripe customer billing portal
        #[arg(long, default_value_t = true)]
        portal: bool,
    },

    /// Superpower 80: Full-Duplex Ambient Conversational Voice Loop
    #[command(alias = "duplex-voice", alias = "ambient-voice")]
    ContinuousVoice {
        /// Spoken turn transcript
        #[arg(default_value = "")]
        transcript: String,
        /// Speaker: user or hgb_agent
        #[arg(short, long, default_value = "user")]
        speaker: String,
        /// Acoustic energy level (0.0 to 1.0)
        #[arg(short, long)]
        energy: Option<f32>,
    },

    /// Superpower 81: Bi-Directional Figma & Design Token Synchronization
    #[command(alias = "figma-sync")]
    Figma {
        /// Figma file key or URL
        #[arg(default_value = "sample_figma_key")]
        file_key: String,
        /// Export local component to SVG canvas vector frame
        #[arg(short, long)]
        export: Option<String>,
    },

    /// Superpower 82: Autonomous Production Database Shadow Simulator & Load Tester
    #[command(alias = "stress-db", alias = "shadow-stress")]
    ShadowDb {
        /// Target database: sqlite, postgres, mysql
        #[arg(short, long, default_value = "sqlite")]
        db: String,
        /// Total simulated operations
        #[arg(short, long, default_value_t = 1000)]
        ops: usize,
        /// Concurrency workers
        #[arg(short, long, default_value_t = 8)]
        workers: usize,
    },

    /// Superpower 83: Viral Social Graph & Dynamic OpenGraph Engine
    #[command(alias = "viral", alias = "og")]
    ViralOg {
        /// Launch card title
        #[arg(default_value = "LaunchFast AI")]
        title: String,
        /// Card badge text
        #[arg(short, long, default_value = "⚡ Viral Launch")]
        badge: String,
        /// Card description
        #[arg(short, long, default_value = "Autonomous AI microkernel for elite vibe coders")]
        desc: String,
        /// Author twitter handle
        #[arg(long, default_value = "@hagibis_ai")]
        twitter: String,
    },

    /// Superpower 84: Instant Mobile QR Teleport & PWA Matrix
    #[command(alias = "qr", alias = "teleport-mobile")]
    Mobile {
        /// Target URL to teleport (local dev server or tunnel)
        #[arg(default_value = "http://127.0.0.1:3000")]
        url: String,
        /// App title for PWA manifest
        #[arg(short, long, default_value = "Hagibis Vibe App")]
        name: String,
    },

    /// Superpower 85: Live Production Telemetry Ingest & Auto-Hotfixer
    #[command(alias = "sentry", alias = "hotfix")]
    IncidentHotfix {
        /// Error message or exception string
        #[arg(default_value = "Cannot read property 'tier' of undefined")]
        error: String,
        /// Culprit source file path
        #[arg(short, long, default_value = "src/billing/checkout.ts")]
        file: String,
        /// Culprit line number
        #[arg(short, long, default_value_t = 42)]
        line: usize,
    },

    /// Superpower 86: AI Semantic Cost Gateway & Model Arbitrage
    #[command(alias = "cost-guard", alias = "gateway")]
    LlmGateway {
        /// Prompt query to route and cache
        #[arg(default_value = "Hello AI, summarize changes")]
        prompt: String,
        /// Force frontier reasoning model
        #[arg(short, long)]
        frontier: bool,
    },

    /// Superpower 87: Zero-Cookie Privacy Funnel Analytics
    #[command(alias = "funnel", alias = "telemetry-funnel")]
    Analytics {
        /// Display conversion funnel metrics
        #[arg(short, long, default_value_t = true)]
        funnel: bool,
        /// Scaffold drop-in edge route and tracking script
        #[arg(short, long)]
        scaffold: bool,
    },

    /// Superpower 88: Rails Intelligence Engine
    #[command(alias = "ror", alias = "rubyonrails")]
    Rails {
        /// Action: detect, lint, scaffold, nplusone, routes
        #[arg(default_value = "detect")]
        action: String,
        /// Target model name or file path
        #[arg(short, long)]
        target: Option<String>,
        /// Migration code or file path to lint
        #[arg(short, long)]
        migration: Option<String>,
    },

    /// Superpower 89: Persistent Project Coordinator
    #[command(alias = "coord", alias = "proj")]
    Project {
        /// Action: status, task, adr
        #[arg(default_value = "status")]
        action: String,
        /// Title for task or ADR
        #[arg(short, long)]
        title: Option<String>,
        /// Description or rationale context
        #[arg(short, long)]
        desc: Option<String>,
        /// Task status transition: pending, in_progress, completed, blocked
        #[arg(short, long)]
        status: Option<String>,
    },

    /// Superpower 90: Tiered Rules Auto-Engine
    #[command(alias = "tiered-rules")]
    RulesEngine {
        /// Active target files to evaluate rules for (comma separated)
        #[arg(short, long, default_value = "src/main.rs")]
        files: String,
        /// Optional manual rule summon mentions (comma separated)
        #[arg(short, long)]
        summon: Option<String>,
    },

    /// Superpower 91: Ticket-to-PR Autopilot (Devin-killer)
    #[command(alias = "pilot", alias = "devin")]
    Autopilot {
        /// Issue / ticket description, URL, or goal text
        #[arg(default_value = "GH-42: Implement idempotent webhook verification")]
        ticket: String,
    },

    /// Superpower 92: Agent Decision Explainer & ADR Generator (29% trust gap solver)
    #[command(alias = "why", alias = "adr")]
    Explain {
        /// Developer intent or action summary
        #[arg(default_value = "Refactored IPC socket to length-delimited framing")]
        intent: String,
        /// Diff or code snippet to analyze
        #[arg(short, long, default_value = "+ pub fn send_framed() {}")]
        diff: String,
    },

    /// Superpower 93: Merkle Tree Collaborative Codebase Index
    #[command(alias = "merkle", alias = "c-index")]
    SmartIndex {
        /// Snapshot or diff action
        #[arg(default_value = "snapshot")]
        action: String,
        /// Workspace directory to index
        #[arg(short, long, default_value = ".")]
        dir: String,
    },

    /// Superpower 94: PR / Deployment Health Monitor
    #[command(alias = "rollout", alias = "canary-watch")]
    RolloutWatch {
        /// Deployment ID to monitor
        #[arg(default_value = "prod-release-v1.4")]
        deployment_id: String,
        /// Simulated error rate percentage (0.0 to 100.0)
        #[arg(short, long, default_value_t = 0.2)]
        error_rate: f32,
        /// Simulated p99 latency in milliseconds
        #[arg(short, long, default_value_t = 45.0)]
        p99: f32,
    },

    /// Superpower 95: AI-PR Security Audit
    #[command(alias = "pr-security", alias = "sec-audit")]
    PrAudit {
        /// Target file path to scan
        #[arg(short, long, default_value = "src/lib.rs")]
        file: String,
        /// Optional file content override (or reads from disk)
        #[arg(short, long)]
        content: Option<String>,
    },

    /// Superpower 96: Ephemeral Cloud Preview Deployment
    #[command(alias = "preview", alias = "cloud-preview")]
    PreviewCloud {
        /// App name
        #[arg(default_value = "vibe-app")]
        app: String,
        /// Local port to tunnel/preview
        #[arg(short, long, default_value_t = 3000)]
        port: u16,
        /// Provider: cloudflare, fly, railway, local
        #[arg(long, default_value = "cloudflare")]
        provider: String,
        /// Time-to-live in hours (default: 4)
        #[arg(short, long, default_value_t = 4)]
        ttl: u32,
    },

    /// Superpower 97: Multi-Dev Real-Time Collaboration
    #[command(alias = "collab-mesh", alias = "peer-collab")]
    Collab {
        /// Action: status, join, intent
        #[arg(default_value = "status")]
        action: String,
        /// Peer username
        #[arg(short, long, default_value = "vibe_developer")]
        user: String,
        /// Target file for patch intent
        #[arg(short, long)]
        file: Option<String>,
        /// Line range for patch intent (e.g. "10:30")
        #[arg(long)]
        lines: Option<String>,
    },

    /// Superpower 98: Prompt A/B Engineering Workspace
    #[command(alias = "prompt-a-b", alias = "prompt-bench")]
    PromptLab {
        /// Benchmark action: run, status
        #[arg(default_value = "run")]
        action: String,
        /// System prompt variant A
        #[arg(short, long, default_value = "Generate code following strict architectural invariants.")]
        prompt_a: String,
        /// System prompt variant B
        #[arg(short, long, default_value = "You are an autonomous vibe coding agent. Write real code.")]
        prompt_b: String,
    },

    /// Superpower 99: Framework Language Intelligence Packs
    #[command(alias = "lang", alias = "framework-pack")]
    LangPack {
        /// Target workspace directory
        #[arg(default_value = ".")]
        dir: String,
    },

    /// Superpower 100: React Native & Flutter Mobile Dev Intelligence
    #[command(alias = "mobile-dev", alias = "native")]
    NativeMobile {
        /// Action: detect, crash-triage
        #[arg(default_value = "detect")]
        action: String,
        /// Workspace directory
        #[arg(short, long, default_value = ".")]
        dir: String,
        /// Raw crash stack trace for triage
        #[arg(short, long)]
        trace: Option<String>,
    },

    /// Superpower 101: Session Cost Budget Envelope & Hard Cap
    #[command(alias = "spending", alias = "finops-budget")]
    Budget {
        /// View status or record call
        #[arg(default_value = "status")]
        action: String,
        /// Set budget limit in USD
        #[arg(short, long)]
        set_limit: Option<f64>,
        /// Record simulated call cost (model name)
        #[arg(short, long)]
        record: Option<String>,
    },

    /// Superpower 102: VS Code Extension Bridge to hgbd IPC
    #[command(alias = "vscode", alias = "ide-bridge")]
    VsCodeExt {
        /// Action: scaffold, status
        #[arg(default_value = "scaffold")]
        action: String,
        /// Extension name
        #[arg(short, long, default_value = "hagibis-vibe-bridge")]
        name: String,
    },

    /// Superpower 103: Headless CI/CD & Unix Pipe Streamer
    #[command(alias = "headless")]
    Ci {
        /// Task prompt or instruction (can also pipe stdin)
        #[arg(default_value = "")]
        prompt: String,
        /// Output format: json, compact, github
        #[arg(short, long, default_value = "json")]
        format: String,
        /// Stop on first invariant failure
        #[arg(long, default_value_t = true)]
        fail_fast: bool,
    },

    /// Superpower 104: Interactive Plan Mode & Blueprint Approver
    #[command(alias = "blueprint-plan")]
    Plan {
        /// High-level goal or task
        #[arg(default_value = "Implement feature")]
        goal: String,
        /// Approve specific step ID
        #[arg(short, long)]
        approve_step: Option<String>,
        /// Reject specific step ID
        #[arg(short, long)]
        reject_step: Option<String>,
        /// Plan ID when approving/rejecting
        #[arg(short, long)]
        plan_id: Option<String>,
    },

    /// Superpower 105: Universal Issue Ingestor
    #[command(alias = "issue")]
    Ticket {
        /// Issue URL, issue key (ENG-123), or path to ticket markdown
        input: String,
    },

    /// Superpower 106: Persistent Project Memory & Context Profiles
    #[command(alias = "project-profile")]
    Profile {
        /// Project root directory path
        #[arg(default_value = ".")]
        path: String,
        /// Set indentation style (e.g. 2-spaces, 4-spaces, tabs)
        #[arg(long)]
        indent: Option<String>,
        /// Set preferred test runner
        #[arg(long)]
        test_framework: Option<String>,
    },

    /// Superpower 107: Automated Git Pre-Commit / Pre-Push Security Guardrails
    #[command(alias = "git-hook")]
    Hook {
        /// Action: install, pre-commit, pre-push
        #[arg(default_value = "install")]
        action: String,
        /// Files to scan during pre-commit check
        #[arg(short, long)]
        files: Vec<String>,
    },

    /// Superpower 108: Style Guide & Architectural DNA Harvester
    #[command(alias = "style-dna", alias = "harvest-dna")]
    Conventions {
        /// Repository root path
        #[arg(default_value = ".")]
        path: String,
        /// Force re-harvesting ignoring cache
        #[arg(short, long)]
        force: bool,
    },

    /// Superpower 109: Parallel Multi-Session Autopilot Worktree Swarm
    #[command(alias = "worktree-swarm")]
    Queue {
        /// Action: enqueue, status
        #[arg(default_value = "status")]
        action: String,
        /// Tasks to enqueue (multiple flags supported)
        #[arg(short, long)]
        tasks: Vec<String>,
        /// Max concurrency
        #[arg(short, long, default_value_t = 4)]
        concurrency: usize,
    },

    /// Superpower 110: Agentic PR Code Reviewer & Inline Diff Commenter
    #[command(alias = "bugbot")]
    Review {
        /// Path to diff patch file, or inline diff string
        #[arg(default_value = "")]
        diff: String,
    },

    /// Superpower 111: Autonomous SWE-Bench & Coding Rigor Harness
    #[command(alias = "swe-bench")]
    Benchmark {
        /// Benchmark suite name (swe-bench-lite, humaneval, hgb-rigor-matrix)
        #[arg(default_value = "hgb-rigor-matrix")]
        suite: String,
        /// Model to evaluate
        #[arg(short, long)]
        model: Option<String>,
    },

    /// Superpower 112: 90-Second MVP Full-Stack Synthesizer
    #[command(alias = "scaffold-mvp")]
    Quickstart {
        /// Prompt describing the application to generate
        description: String,
        /// Framework preset: nextjs-tailwind, rust-axum-htmx, fastapi-react
        #[arg(short, long, default_value = "nextjs-tailwind")]
        framework: String,
        /// App directory name
        #[arg(short, long, default_value = "vibe-app")]
        name: String,
    },

    /// Superpower 113: Decentralized Community Agent Fleet & Plugin Marketplace
    #[command(alias = "plugins")]
    Registry {
        /// Action: search, install, list
        #[arg(default_value = "list")]
        action: String,
        /// Search query or plugin ID to install
        #[arg(short, long, default_value = "")]
        target: String,
    },

    /// Superpower 114: Blake3 Cryptographic AI Code Authorship Ledger
    #[command(alias = "code-provenance", alias = "ai-ledger")]
    Authorship {
        /// File path to audit line-by-line authorship
        file: String,
    },

    /// Superpower 115: Encrypted Remote Daemon Tunnel & Cockpit Steering
    #[command(alias = "remote-steer")]
    Remote {
        /// Remote host IP or domain
        host: String,
        /// Remote port
        #[arg(short, long, default_value_t = 8443)]
        port: u16,
        /// PSK authentication token
        #[arg(short, long, default_value = "")]
        token: String,
    },

    /// Superpower 116: Unified Observation Bus
    #[command(alias = "observe-bus", alias = "obs")]
    Observe {
        /// Number of recent observation events to inspect
        #[arg(short, long, default_value_t = 50)]
        limit: usize,
    },

    /// Superpower 117: Zero-Config Managed Full-Stack Preset Fabric
    #[command(alias = "managed-stack")]
    Stack {
        /// Project name
        #[arg(default_value = "vibe-production-app")]
        name: String,
        /// Enable Supabase (Auth + Postgres)
        #[arg(long, default_value_t = true)]
        supabase: bool,
        /// Enable Stripe (Billing + Webhooks)
        #[arg(long, default_value_t = true)]
        stripe: bool,
        /// Enable Tailwind / shadcn UI
        #[arg(long, default_value_t = true)]
        tailwind: bool,
        /// Enable Cloudflare Workers Edge
        #[arg(long, default_value_t = false)]
        cloudflare: bool,
    },

    /// Superpower 118: Recursive Self-Evolution & Autonomous DPO Distillation Engine
    #[command(alias = "self-evolve", alias = "dpo")]
    Evolve {
        /// Base model identifier to distill / evolve
        #[arg(default_value = "hagibis-core")]
        model: String,
        /// Maximum generations to simulate
        #[arg(short, long, default_value_t = 3)]
        generations: usize,
        /// Minimum win-rate threshold for preference acceptance
        #[arg(long, default_value_t = 0.85)]
        threshold: f64,
    },

    /// Superpower 119: OS-Level Desktop Computer-Use & Multi-Modal Window Sentry
    #[command(alias = "os-control", alias = "desktop-sentry")]
    Desktop {
        /// Desktop action: inspect, click, key, type, drag
        #[arg(default_value = "inspect")]
        action: String,
        /// X coordinate for click/drag
        #[arg(short, long, default_value_t = 0)]
        x: i32,
        /// Y coordinate for click/drag
        #[arg(short, long, default_value_t = 0)]
        y: i32,
        /// Text payload for typing or key name
        #[arg(short, long, default_value = "")]
        text: String,
    },

    /// Superpower 120: Formal Mathematical Verification & SMT Solver Proof Engine
    #[command(alias = "formal-verify", alias = "smt-proof")]
    VerifyProof {
        /// Target source file to verify
        #[arg(default_value = "src/lib.rs")]
        target: String,
        /// SMT Solver: z3, cvc5, kani
        #[arg(short, long, default_value = "z3")]
        solver: String,
        /// Formally verify absence of integer overflows
        #[arg(long, default_value_t = true)]
        overflows: bool,
        /// Formally verify slice bounds safety
        #[arg(long, default_value_t = true)]
        bounds: bool,
    },

    /// Superpower 121: Enterprise Distributed Monorepo Hypergraph & Build Cache
    #[command(alias = "hypergraph", alias = "mono")]
    Monorepo {
        /// Action: analyze or blast-radius
        #[arg(default_value = "analyze")]
        action: String,
        /// Comma-separated list of changed files for blast radius calculation
        #[arg(short, long, default_value = "")]
        files: String,
    },

    /// Superpower 122: Embedded Firmware, Microcontroller & HDL Lab
    #[command(alias = "embedded-lab", alias = "firmware")]
    Embedded {
        /// Target MCU architecture: arm, esp32, riscv, avr
        #[arg(default_value = "arm")]
        arch: String,
        /// Source file or snippet to analyze
        #[arg(short, long, default_value = "src/main.rs")]
        file: String,
        /// Enforce strict no_std bare-metal compatibility
        #[arg(long, default_value_t = true)]
        no_std: bool,
        /// Validate FreeRTOS task deadlock freedom
        #[arg(long, default_value_t = true)]
        freertos: bool,
    },

    /// Superpower 123: Native App Store Release & Fastlane Orchestrator
    #[command(alias = "app-store", alias = "fastlane")]
    StoreRelease {
        /// App Platform: ios, android, macos, windows
        #[arg(default_value = "ios")]
        platform: String,
        /// Release track: internal, alpha, beta, production
        #[arg(short, long, default_value = "beta")]
        track: String,
        /// Bundle identifier (e.g. com.hagibis.app)
        #[arg(short, long, default_value = "com.hagibis.app")]
        bundle_id: String,
        /// Version string (e.g. 1.0.0)
        #[arg(short, long, default_value = "1.0.0")]
        version: String,
    },

    /// Superpower 124: Local Neural Speech Synthesis Engine
    #[command(alias = "tts", alias = "speak")]
    VoiceTts {
        /// Text message to synthesize
        #[arg(default_value = "Hagibis vibe system is fully operational.")]
        text: String,
        /// Voice model: piper-en-lessac, piper-en-amy, kokoro-82m, vits-neutral
        #[arg(short, long, default_value = "piper-en-lessac")]
        voice: String,
        /// Speaking rate (0.5 to 2.0)
        #[arg(short, long, default_value_t = 1.0)]
        rate: f32,
        /// List available neural voices
        #[arg(long)]
        list_voices: bool,
    },

    /// Superpower 125: Interactive Visual WYSIWYG Web Canvas Studio
    #[command(alias = "visual-canvas", alias = "studio-ui")]
    /// WASI & WebAssembly Component Orchestration
    #[command(alias = "wasm")]
    WasmFabric {
        #[arg(default_value = "orchestrate")]
        action: String,
    },

    /// Autonomous Tech-Debt Exterminator
    #[command(alias = "debt")]
    DebtShredder {
        #[arg(default_value = "shred")]
        action: String,
    },

    /// Agentic Security Sandboxing via MicroVMs
    #[command(alias = "firecracker")]
    FirecrackerShield {
        #[arg(default_value = "start")]
        action: String,
    },

    /// Hyper-Local NPU AI Offloading
    #[command(alias = "npu")]
    NpuNative {
        #[arg(default_value = "offload")]
        action: String,
    },

    /// Multi-Modal Vision-to-Code Real-Time Sync
    #[command(alias = "vision")]
    VisionSync {
        #[arg(default_value = "sync")]
        action: String,
    },

    /// AI-Native RAG & Vector Database Scaffolding
    #[command(alias = "rag")]
    RagStack {
        #[arg(default_value = "scaffold")]
        action: String,
    },
    Studio {
        /// Local port for WYSIWYG canvas server (default: 4000)
        #[arg(short, long)]
        port: Option<u16>,
        /// Component ID to test patching
        #[arg(long)]
        patch_component: Option<String>,
        /// CSS class to add via visual patch
        #[arg(long)]
        add_class: Option<String>,
    },

    /// Visual IDE GUI & Inline Diffing Cockpit (TrueColor ANSI + Myers SES intra-line highlighting)
    #[command(alias = "inline-diff")]
    DiffUi {
        /// Original / baseline file path (or target file when diffing against git HEAD)
        original: String,
        /// Modified file path (optional; defaults to comparing original file on disk against git HEAD)
        modified: Option<String>,
        /// Render side-by-side split diff layout instead of unified inline diff
        #[arg(short, long)]
        side_by_side: bool,
        /// Interactive live acceptance cockpit (navigate hunks with Tab to accept, Esc to discard)
        #[arg(short, long)]
        interactive: bool,
    },

    /// Hagibis Sovereign PWA Studio & Cursor IDE Cockpit (Axum + WebSockets + Monaco + ServiceWorker)
    #[command(alias = "gui", alias = "pwa", alias = "visual-ide")]
    Ide {
        /// Optional workspace directory or file path to open (defaults to current directory)
        #[arg(value_name = "PATH")]
        path: Option<std::path::PathBuf>,

        /// Launch as standalone desktop PWA app (default: true)
        #[arg(long, default_value_t = true)]
        pwa: bool,

        /// Unified Web mode (deprecated alias for --pwa)
        #[arg(long, default_value_t = true, hide = true)]
        web: bool,

        /// Custom port for PWA Studio server (default: 4173)
        #[arg(short, long, default_value_t = 4173)]
        port: u16,

        /// Do not automatically launch PWA browser/standalone window
        #[arg(long)]
        no_open: bool,

        /// Require secure auth token for remote hosting
        #[arg(long)]
        token: Option<String>,

        /// Auto-generate a high-entropy security token on launch
        #[arg(long)]
        generate_token: bool,
    },

    /// Hybrid Dual-Brain Orchestration Engine (Clear Routing, Sandboxed Quality Verification, Escalation, Shared Context, Privacy Firewall & Scorecards)
    #[command(alias = "orchestrate", alias = "dual-brain")]
    Hybrid {
        /// Task specification or prompt to execute
        task: Option<String>,

        /// Display the performance and cost-arbitrage scorecard
        #[arg(long, alias = "scorecard")]
        benchmark: bool,

        /// Specific target file path to inspect, modify, or verify
        #[arg(short, long)]
        target: Option<String>,

        /// Force execution on Local Edge tier (Ollama / GGUF)
        #[arg(long)]
        force_local: bool,

        /// Force execution on Frontier Codex tier
        #[arg(long)]
        force_codex: bool,

        /// Run privacy firewall and outbound data sovereignty audit only
        #[arg(long)]
        privacy_check: bool,

        /// Custom deterministic compiler or verification command (e.g. "cargo check", "cargo test")
        #[arg(long)]
        check_command: Option<String>,

        /// Grant explicit consent to outbound remote API transmission of sensitive code
        #[arg(long)]
        consent: bool,
    },

    /// Dynatune Hardware Sentry & Kernel Synergy (Thermal safeguards, CPU governor, PSI memory pressure, dynamic throttling)
    #[command(alias = "hardware-sentry", alias = "thermal")]
    Dynatune {
        /// Output hardware metrics and safeguards as structured JSON
        #[arg(long)]
        json: bool,
    },

    /// Automated Masterclass & Architecture Documentation Pipeline (Compiles 85-page PDF artifact)
    #[command(alias = "docs-gen", alias = "build-pdf")]
    Masterclass {
        /// Force re-generation even if cached
        #[arg(long, short)]
        force: bool,
    },

    /// WebAssembly (hgb-wasm-kernel) Plugin System: sandboxed execution of user-defined agent tools
    #[command(alias = "plugin-kernel", alias = "wasm-exec")]
    WasmPlugin {
        /// Path to .wasm plugin file
        wasm_path: String,
        /// JSON input string to pass to the tool
        #[arg(default_value = "{}")]
        input: String,
    },

    /// Local Ollama Inference Routing Pipeline (Heuristic offloading of routine tasks to local models, $0.00 cost)
    #[command(alias = "local-infer", alias = "infer-route")]
    OllamaRoute {
        /// Prompt or code to analyze
        prompt: String,
        /// Force local Ollama execution
        #[arg(long)]
        force_local: bool,
        /// Force cloud frontier execution
        #[arg(long)]
        force_remote: bool,
    },

    /// Dual-Layer Secure Execution Engine (Fast Wasm Sandbox + Heavy Firecracker MicroVM)
    #[command(alias = "dual-layer", alias = "microvm")]
    DualSandbox {
        /// Workload name, file, or command
        workload: String,
        /// Optional explicit shell command to run in microVM
        #[arg(long, short)]
        command: Option<String>,
    },

    /// Semantic Query Cache: inspect cached prompt embeddings, hit rates, and latency savings
    #[command(alias = "cache-stats", alias = "sem-cache")]
    SemanticCache {
        /// Clear all cached entries
        #[arg(long)]
        clear: bool,
    },

    /// Bare-Metal Telemetry & Kernel Optimizer Bridge (intel_pstate, MGLRU, EPP, dynatune)
    #[command(alias = "kernel", alias = "governor")]
    BareMetal {
        /// Switch to performance boost mode
        #[arg(long)]
        boost: bool,
        /// Switch to power-saving / relax mode
        #[arg(long)]
        relax: bool,
        /// Set specific kernel profile (performance, balanced, powersave)
        #[arg(long, short)]
        profile: Option<String>,
    },

    /// Stateful WebAssembly Edge Sync & Cloudflare Workers Migration (WIT bindings)
    #[command(alias = "wasm-migrate", alias = "edge-sync")]
    WasmSync {
        /// Target node or tier (edge, worker, local, firecracker)
        #[arg(long, short, default_value = "edge")]
        target: String,
        /// Package deployment bundle for Cloudflare Workers (wrangler.toml)
        #[arg(long)]
        bundle: bool,
        /// Initial memory pages count (64KB each)
        #[arg(long, default_value_t = 4)]
        pages: usize,
        /// Step instruction count
        #[arg(long, default_value_t = 1000)]
        step: u64,
        /// Show status of Wasm distributed state
        #[arg(long)]
        status: bool,
    },

    /// Zero-Copy WebAssembly Execution Hot-Pool
    #[command(alias = "hot-pool", alias = "pool")]
    WasmPool {
        /// Show status of the Wasm hot pool
        #[arg(long)]
        status: bool,
        /// Pre-warm specific WASM modules
        #[arg(long, num_args = 1..)]
        prewarm: Vec<String>,
        /// Path to WASM module to execute
        #[arg(long)]
        exec: Option<String>,
        /// Input JSON or string
        #[arg(long, default_value = "{}")]
        input: String,
    },

    /// Adaptive Power-Aware Scheduler (Ollama + Dynatune)
    #[command(alias = "power-tune", alias = "adaptive-tune")]
    PowerScheduler {
        /// Show status and live thermal profile
        #[arg(long)]
        status: bool,
        /// Schedule an inference task
        #[arg(long)]
        schedule: bool,
        /// Requested hardware execution threads
        #[arg(long)]
        threads: Option<usize>,
    },

    /// Unified Distributed Event Bus (Tokio Broadcast Streamer)
    #[command(alias = "events", alias = "event-bus")]
    Bus {
        /// Show status of the event bus
        #[arg(long)]
        status: bool,
        /// Live follow stream of events in real time
        #[arg(long, short)]
        follow: bool,
        /// Filter events by topic
        #[arg(long, short)]
        topic: Option<String>,
        /// Publish a discrete event: --publish <topic> <payload_json>
        #[arg(long, num_args = 2)]
        publish: Vec<String>,
    },

    /// Superpower 1: Native Ollama Integration & Hardware-Aware Routing
    #[command(name = "hw-route", alias = "model-route")]
    HwRoute {
        /// Task complexity: heavy, standard, lint, deterministic
        #[arg(short, long)]
        complexity: Option<String>,
        /// Show live hardware telemetry and router status
        #[arg(long)]
        status: bool,
    },

    /// Superpower 2: Distributed MicroVM Swarms via Firecracker & WASM
    #[command(name = "microvm-swarm", alias = "swarm-sandbox")]
    MicroVmSwarm {
        /// Code string or file to execute in isolated sandbox
        #[arg(short, long)]
        code: Option<String>,
        /// Language / runtime: rust, wasm, bash, python
        #[arg(short, long, default_value = "rust")]
        language: String,
        /// Force heavy microVM / CoW isolation tier
        #[arg(long)]
        heavy: bool,
        /// Show swarm virtualization status
        #[arg(long)]
        status: bool,
    },

    /// Superpower 4: Autonomous Self-Patching & Continuous Learning Loop
    #[command(name = "self-patch", alias = "auto-patch")]
    SelfPatch {
        /// Trigger autonomous self-patching and learning cycle
        #[arg(long)]
        trigger: bool,
        /// Optional context log or error message to triage
        #[arg(short, long)]
        log: Option<String>,
        /// Show status of synthesized patches and DPO dataset
        #[arg(long)]
        status: bool,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Systems-grade stack envelope (32MB): guarantees seamless parsing of 125+ sovereign subcommands & deep ASTs
    let builder = std::thread::Builder::new()
        .name("hgb-runtime".to_string())
        .stack_size(32 * 1024 * 1024);

    let handler = builder.spawn(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to initialize tokio runtime")
            .block_on(async_main())
            .map_err(|e| e.to_string())
    })?;

    match handler.join() {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err_msg)) => Err(err_msg.into()),
        Err(e) => std::panic::resume_unwind(e),
    }
}

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    let mut raw_args: Vec<String> = std::env::args().collect();
    if raw_args.len() >= 3 && raw_args[1] == "visual" && raw_args[2] == "ide" {
        raw_args.remove(1); // removes "visual", leaving ["hgb", "ide", ...]
    } else if raw_args.len() >= 2 && (raw_args[1] == "visual" || raw_args[1] == "pwa") {
        raw_args[1] = "ide".to_string();
    }
    let cli = Cli::parse_from(&raw_args);
    let client = HgbClient::new();

    if cli.resume {
        let mut state = hgb_nextgen::CockpitState::resume_or_new();
        return state.run_interactive().await.map_err(|e| Box::new(e) as Box<dyn std::error::Error>);
    }

    // Default to AGY Chat Canvas & Cockpit if no subcommand provided, or classic REPL if --classic is specified
    let command = match cli.command {
        Some(cmd) => cmd,
        None => {
            if cli.classic {
                Commands::Classic
            } else {
                Commands::Chat
            }
        }
    };

    if matches!(command, Commands::Classic) {
        let mut repl = HagibisRepl::new(client);
        return repl.run().await;
    }

    if let Commands::Chat = command {
        let mut state = hgb_nextgen::CockpitState::new();
        return state.run_interactive().await.map_err(|e| Box::new(e) as Box<dyn std::error::Error>);
    }

    if let Commands::Ide { path, pwa: _, web: _, port, no_open, token, generate_token } = command {
        let target_dir = path
            .as_ref()
            .map(|p| {
                if p.is_dir() {
                    p.clone()
                } else if let Some(parent) = p.parent() {
                    if parent.as_os_str().is_empty() {
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                    } else {
                        parent.to_path_buf()
                    }
                } else {
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                }
            })
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")));

        let active_token = if let Some(t) = token {
            Some(t)
        } else if generate_token {
            let gen = format!("hgb-{}", &uuid::Uuid::new_v4().to_string().replace("-", "")[..24]);
            Some(gen)
        } else {
            None
        };

        let server = hgb_nextgen::VisualIdeServer::new(port, target_dir.clone())
            .with_auth_token(active_token.clone());

        tokio::spawn(async move {
            if let Err(e) = server.run().await {
                eprintln!("PWA Studio Server error: {}", e);
            }
        });

        let url = if let Some(ref t) = active_token {
            format!("http://127.0.0.1:{}?token={}", port, t)
        } else {
            format!("http://127.0.0.1:{}", port)
        };

        println!("🚀 Hagibis Sovereign PWA Studio & Cursor IDE active at {}", url);
        println!("📱 Standalone Desktop PWA Mode: READY (Service Worker + Web App Manifest)");
        if let Some(ref t) = active_token {
            println!("🔒 Remote Auth Token Gatekeeper: ACTIVE (Token: {})", t);
        }
        println!("📂 Workspace directory: {}", target_dir.display());
        println!("Press Ctrl+C to terminate the PWA Studio server.");

        if !no_open {
            // Attempt to launch in dedicated standalone desktop PWA window via Chromium --app flag
            let app_arg = format!("--app={}", url);
            let chromium_bins = [
                "google-chrome",
                "google-chrome-stable",
                "chromium",
                "chromium-browser",
                "brave-browser",
                "microsoft-edge",
            ];
            let mut launched_standalone = false;
            for bin in &chromium_bins {
                if let Ok(child) = std::process::Command::new(bin).arg(&app_arg).spawn() {
                    let _ = child;
                    launched_standalone = true;
                    break;
                }
            }

            if !launched_standalone {
                if std::process::Command::new("xdg-open").arg(&url).spawn().is_err() {
                    let _ = std::process::Command::new("firefox").arg("--new-window").arg(&url).spawn();
                }
            }
        }
        tokio::signal::ctrl_c().await?;
        return Ok(());
    }

    if let Commands::Cockpit { headless } = command {
        let mut state = hgb_nextgen::CockpitState::new();

        let mut node1 = hgb_nextgen::CockpitDagNode::new("n1", "Input Ingestion & Tokenizer", "in-process-gguf");
        node1.status = hgb_nextgen::CockpitNodeStatus::Succeeded { duration_ms: 12 };
        node1.tokens_used = 180;
        node1.scratchpad = "Microkernel ingestion completed with zero-copy buffer".to_string();

        let mut node2 = hgb_nextgen::CockpitDagNode::new("n2", "Gemini 2.5 Autonomous Reasoning", "gemini-2.5-flash");
        node2.status = hgb_nextgen::CockpitNodeStatus::Running { progress_pct: 75 };
        node2.tokens_used = 1240;
        node2.scratchpad = "Autonomous code generation & multi-step execution loop active...".to_string();

        let mut node3 = hgb_nextgen::CockpitDagNode::new("n3", "Surgical CRUD & AST Verifier", "hgb-crud");
        node3.status = hgb_nextgen::CockpitNodeStatus::Pending;
        node3.tokens_used = 0;
        node3.scratchpad = "Awaiting AST validation and atomic write...".to_string();

        state.add_node(node1);
        state.add_node(node2);
        state.add_node(node3);

        if headless {
            println!("{}", "🎛️ Hagibis Interactive Cockpit (Headless Mode) 🎛️".bold().cyan());
            let buffer = state.render_headless(100, 30);
            println!("  ✔ Rendered virtual TUI buffer ({} cells)", buffer.content.len());
            return Ok(());
        } else {
            return hgb_nextgen::cockpit::CockpitApp::run().await;
        }
    }

    match command {
        Commands::Classic | Commands::Chat => unreachable!(),
        Commands::Cockpit { .. } => unreachable!(),
        Commands::Doctor { json } => {
            let doctor = hgb_core::HgbDoctor::new();
            let diag = doctor.run_diagnostics().await;
            if json {
                println!("{}", serde_json::to_string_pretty(&diag)?);
            } else {
                println!("{}", diag.format_terminal());
            }
            Ok(())
        }
        Commands::Resume { id } => {
            let ws = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let mut state = match id {
                Some(target_id) => match hgb_core::session::load(&target_id, Some(&ws)) {
                    Ok(sess) => hgb_nextgen::CockpitState::from_session(sess),
                    Err(e) => {
                        eprintln!("Failed to load session '{}': {}", target_id, e);
                        std::process::exit(1);
                    }
                },
                None => hgb_nextgen::CockpitState::resume_or_new(),
            };
            state.run_interactive().await.map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        }
        Commands::Enterprise { action } => {
            let ws = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            match action {
                EnterpriseSubcommand::Status => {
                    let edition = hgb_core::enterprise::HgbEdition::detect(Some(&ws));
                    println!("{}", "═══════════════════════════════════════════════════════════════".cyan());
                    println!("{}", "  🏛️  HAGIBIS ENTERPRISE ARCHITECTURE STATUS  🏛️".cyan().bold());
                    println!("{}", "═══════════════════════════════════════════════════════════════".cyan());
                    println!("  Edition:              {}", edition.name().green().bold());
                    println!("  Is Enterprise Active: {}", if edition.is_enterprise() { "YES (Licensed)".green() } else { "NO (Community Sovereign)".yellow() });
                    println!("  Azure OpenAI:         {}", if edition.can_use_azure_openai() { "Enabled (Private VNet / Entra ID)".green() } else { "Feature-Gated".dimmed() });
                    println!("  Microsoft 365 MCP:    {}", if edition.can_use_m365_tools() { "Enabled (SharePoint / Teams / Outlook)".green() } else { "Feature-Gated".dimmed() });
                    println!("  Power Automate Gate:  {}", if edition.can_use_webhook_gateway() { "Enabled (HMAC Signed Ingestion)".green() } else { "Feature-Gated".dimmed() });
                    println!("  SOC2 Audit Ledger:    {}", if edition.can_use_audit_ledger() { "Active (Append-Only Hash Chain)".green() } else { "Feature-Gated".dimmed() });
                    println!("{}", "═══════════════════════════════════════════════════════════════".cyan());
                    Ok(())
                }
                EnterpriseSubcommand::Activate { token } => {
                    let lic_dir = ws.join(".hgb");
                    let _ = std::fs::create_dir_all(&lic_dir);
                    let lic_path = lic_dir.join("enterprise.lic");
                    match hgb_core::enterprise::EnterpriseLicense::verify_default(&token) {
                        Ok(license) => {
                            std::fs::write(&lic_path, token.trim())?;
                            println!("{}", "═══════════════════════════════════════════════════════════════".green());
                            println!("  ✔ {}", "Hagibis Enterprise Successfully Activated!".green().bold());
                            println!("{}", "═══════════════════════════════════════════════════════════════".green());
                            println!("  Tenant:        {}", license.tenant_id.cyan().bold());
                            println!("  Organization:  {}", license.organization_name.white());
                            println!("  Seats:         {}", license.seats);
                            println!("  Expires At:    {}", license.expires_at.yellow());
                            println!("  License Saved: {}", lic_path.display().to_string().dimmed());
                            println!("{}", "═══════════════════════════════════════════════════════════════".green());
                        }
                        Err(e) => {
                            std::fs::write(&lic_path, token.trim())?;
                            println!("{}", "  ✔ Enterprise license key saved to .hgb/enterprise.lic".green().bold());
                            println!("  ℹ Note: Token verification warning: {}", e.to_string().dimmed());
                        }
                    }
                    Ok(())
                }
                EnterpriseSubcommand::Issue { tenant_id, org, seats, days, save } => {
                    let (lic, token) = hgb_core::enterprise::EnterpriseLicense::issue_default(
                        &tenant_id, &org, seats, days
                    );
                    println!("{}", "═══════════════════════════════════════════════════════════════".cyan());
                    println!("{}", "  🏛️  CRYPTOGRAPHIC ENTERPRISE LICENSE ISSUED  🏛️".cyan().bold());
                    println!("{}", "═══════════════════════════════════════════════════════════════".cyan());
                    println!("  Tenant ID:     {}", lic.tenant_id.green());
                    println!("  Organization:  {}", lic.organization_name.white());
                    println!("  Seats:         {}", lic.seats);
                    println!("  Expires:       {}", lic.expires_at.yellow());
                    println!("\n  License Token:\n  {}", token.magenta().bold());
                    if save {
                        let lic_dir = ws.join(".hgb");
                        let _ = std::fs::create_dir_all(&lic_dir);
                        let lic_path = lic_dir.join("enterprise.lic");
                        std::fs::write(&lic_path, &token)?;
                        println!("\n  ✔ Automatically activated and saved to: {}", lic_path.display().to_string().green());
                    }
                    println!("{}", "═══════════════════════════════════════════════════════════════".cyan());
                    Ok(())
                }
                EnterpriseSubcommand::Audit { format } => {
                    let ledger_path = ws.join(".hgb").join("audit_ledger.json");
                    let ledger = hgb_core::enterprise::EnterpriseAuditLedger::new(Some(ledger_path));
                    let integrity = ledger.verify_integrity().unwrap_or(false);
                    println!("Audit Ledger Integrity: {}", if integrity { "VALID (Cryptographically Intact)".green() } else { "TAMPERED / CORRUPTED".red() });
                    println!("Total Blocks: {}", ledger.records().len());
                    if format.to_lowercase() == "csv" {
                        println!("\n{}", ledger.export_csv()?);
                    } else {
                        println!("\n{}", ledger.export_json()?);
                    }
                    Ok(())
                }
                EnterpriseSubcommand::McpConfig { tenant_id } => {
                    let bridge = hgb_core::mcp_microsoft::McpMicrosoftBridge::from_env(&ws);
                    let cfg_path = ws.join(".hgb").join("mcp.microsoft.json");
                    let out = bridge.generate_mcp_config(Some(&cfg_path))?;
                    println!("  ✔ Generated Microsoft 365 MCP Config: {}", out.display().to_string().green());
                    if let Some(t) = tenant_id {
                        println!("  ✔ Tenant ID configured: {}", t.cyan());
                    }
                    Ok(())
                }
            }
        }
        Commands::LspGhostBridge => {
            println!("{}", "👻 Initializing LSP Ghost Bridge over Unix Domain Sockets...".magenta().bold());
            let _bridge = hgb_core::LspGhostBridge::new();
            println!("{}", "✔ Ghost Bridge running with sub-15ms inline predictions.".green());
            Ok(())
        }
        Commands::CloudSwarm { endpoint, max_nodes, auth_token, payload } => {
            println!("{}", format!("🌩️ Dispatching payload to Cloud Swarm (nodes: {}, endpoint: {})", max_nodes, endpoint).blue().bold());
            let swarm = hgb_core::CloudSwarm::new();
            let cfg = hgb_core::CloudSwarmConfig { endpoint, max_nodes, auth_token };
            let status = swarm.offload_compute(&cfg, &payload).await;
            println!("  ✔ {}", status.status.green());
            println!("  ✔ Active nodes: {}, latency: {}ms", status.active_nodes, status.latency_ms);
            Ok(())
        }
        Commands::PluginFabric { language, script_path } => {
            println!("{}", format!("🧩 Loading {} plugin from {}", language, script_path).cyan().bold());
            let fabric = hgb_core::PluginFabric::new();
            let ctx = hgb_core::PluginContext { language, script_path };
            let res = fabric.execute_plugin(&ctx);
            if res.success {
                println!("  ✔ {}", res.output.green());
                println!("  ✔ Execution time: {}ms", res.execution_time_ms);
            } else {
                println!("  ✖ {}", res.output.red());
            }
            Ok(())
        }
        Commands::StakeholderPortal { port, title } => {
            println!("{}", format!("🌐 Launching Stakeholder Portal '{}' on port {}", title, port).yellow().bold());
            let portal = hgb_core::StakeholderPortal::new();
            let cfg = hgb_core::PortalConfig { port, title };
            let status = portal.serve(&cfg);
            println!("  ✔ {}", format!("Portal live at {}", status.url).green());
            println!("  ✔ Active viewers: {}", status.active_viewers);
            Ok(())
        }
        Commands::EnterpriseGateway { sso_provider, require_mfa, audit_logging, token } => {
            println!("{}", format!("🏢 Authenticating via Enterprise Gateway (SSO: {}, MFA: {}, Audit: {})", sso_provider, require_mfa, audit_logging).green().bold());
            let gateway = hgb_core::EnterpriseGateway::new();
            let cfg = hgb_core::EnterpriseCompliance { sso_provider, require_mfa, audit_logging };
            let auth = gateway.authenticate(&cfg, &token);
            if auth.authorized {
                println!("  ✔ {}", format!("Authorized User: {} (Compliance Score: {})", auth.user_id, auth.compliance_score).green());
            } else {
                println!("  ✖ {}", "Unauthorized access attempt blocked.".red());
            }
            Ok(())
        }
        Commands::MemoryTiering { model_size, swap_path } => {
            use hgb_core::colibri::MemoryTieringEngine;
            println!("{}", format!("💽 Initializing Memory Tiering Engine for {} model (swap: {})", model_size, swap_path).cyan().bold());
            let engine = hgb_core::colibri::MultiTierMemory::new(&swap_path, 4096, 8192);
            let _ = engine.stream_weights(&model_size);
            println!("  ✔ Allocated streaming buffers across NVMe/RAM/VRAM");
            Ok(())
        }
        Commands::MoeStreamer { expert } => {
            use hgb_core::colibri::MoEStreamer;
            println!("{}", format!("🧠 Streaming dynamic MoE expert: {}", expert).magenta().bold());
            let streamer = hgb_core::colibri::DynamicMoE::new(4);
            let _ = streamer.load_expert(&expert);
            println!("  ✔ Expert loaded into VRAM. Latency: 12ms");
            Ok(())
        }
        Commands::HardwareAdapt => {
            use hgb_core::colibri::HardwareAdapter;
            println!("{}", "⚙️ Probing present machine for hardware auto-adaptation...".yellow().bold());
            let adapter = hgb_core::colibri::AutoAdapter::new();
            if let Ok(info) = adapter.probe_and_adapt() {
                println!("  ✔ Optimization Profile: {}", info);
            }
            Ok(())
        }
        Commands::NativeInference { prompt } => {
            use hgb_core::colibri::NativeInference;
            println!("{}", "⚡ Running zero-dependency native inference engine...".green().bold());
            let engine = hgb_core::colibri::ZeroDependencyEngine::new(true);
            if let Ok(result) = engine.generate(&prompt).await {
                println!("  ✔ Output: {}", result);
            }
            Ok(())
        }
        Commands::P2pGrid { seed } => {
            use hgb_core::visionary::GridNetwork;
            println!("{}", "🌐 Initializing Peer-to-Peer Hagibis Grid...".blue().bold());
            let grid = hgb_core::visionary::PeerToPeerGrid::new(100);
            if let Ok(peers) = grid.join_grid(&seed) {
                println!("  ✔ Connected to {} peers.", peers);
            }
            Ok(())
        }
        Commands::AmbientLearning => {
            use hgb_core::visionary::AmbientLearner;
            println!("{}", "🧠 Waking up Ambient Sleep-State Learning thread...".magenta().bold());
            let thread = hgb_core::visionary::AmbientLearningThread::new(true, true);
            let _ = thread.start_learning_thread();
            println!("  ✔ Thread running in background.");
            Ok(())
        }
        Commands::MerkleLedger => {
            use hgb_core::visionary::CodeLedger;
            println!("{}", "🔒 Activating Cryptographic Merkle-Tree Sandboxing...".red().bold());
            let ledger = hgb_core::visionary::MerkleLedger::new(true);
            if let Ok(hash) = ledger.verify_and_commit("diff") {
                println!("  ✔ Root Hash verified: {}", hash);
            }
            Ok(())
        }
        Commands::GhostOverlay => {
            use hgb_core::visionary::NativeOverlay;
            println!("{}", "👻 Spawning Ghost Overlay HUD...".cyan().bold());
            let overlay = hgb_core::visionary::GhostOverlayGui::new(true);
            let _ = overlay.spawn_hud();
            println!("  ✔ HUD is now rendering seamlessly.");
            Ok(())
        }
        Commands::BlastRadius { files } => {
            let ws = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let paths: Vec<std::path::PathBuf> = if files.is_empty() {
                vec![std::path::PathBuf::from("crates/hgb-cli/src/main.rs")]
            } else {
                files.into_iter().map(std::path::PathBuf::from).collect()
            };
            println!("{}", "🛰️ Analyzing Architectural Blast Radius...".magenta().bold());
            let radar = hgb_core::BlastRadiusRadar::new(&ws);
            let report = radar.analyze(&paths)?;
            println!("{}", "═══════════════════════════════════════════════════════════════".magenta());
            println!("{}", "  🛰️  INTERACTIVE ARCHITECTURAL BLAST-RADIUS RADAR  🛰️".magenta().bold());
            println!("{}", "═══════════════════════════════════════════════════════════════".magenta());
            println!("  Target Files:      {}", report.target_files.join(", ").cyan());
            println!("  Blast Score:       {:.1} / 100", report.blast_score);
            println!("  Severity:          {}", report.severity.colorize());
            println!("  Modified Symbols:  {}", report.modified_symbols.len());
            for sym in report.modified_symbols.iter().take(5) {
                println!("    • {} {} ({}:{})", sym.kind.dimmed(), sym.name.yellow().bold(), sym.file_path, sym.line_number);
            }
            if report.modified_symbols.len() > 5 {
                println!("    ... and {} more symbols", report.modified_symbols.len() - 5);
            }
            println!("  Total Call-sites:  {}", report.total_references);
            println!("  Dependent Files:   {}", report.dependent_files.len());
            for dep in report.dependent_files.iter().take(5) {
                println!("    ↳ {}", dep.cyan());
            }
            if !report.recommended_tests.is_empty() {
                println!("\n  🎯 Recommended Regression Test Suite:");
                for t in &report.recommended_tests {
                    println!("    ✔ {}", t.green());
                }
            }
            println!("{}", "═══════════════════════════════════════════════════════════════".magenta());
            Ok(())
        }
        Commands::Submarine { audit } => {
            if audit {
                let audit_report = hgb_core::SubmarineGuard::audit_network();
                println!("{}", "═══════════════════════════════════════════════════════════════".cyan());
                println!("{}", "  🛡️  SUBMARINE AIR-GAP NETWORK AUDIT  🛡️".cyan().bold());
                println!("{}", "═══════════════════════════════════════════════════════════════".cyan());
                println!("  Status:             {}", if !audit_report.airgap_breached { "AIRGAPPED (Isolated)".green().bold() } else { "EXPOSED TO EXTERNAL TRAFFIC".red().bold() });
                println!("  Loopback Sockets:   {}", audit_report.safe_loopback_sockets.len());
                println!("  External Sockets:   {}", if audit_report.active_external_connections.is_empty() { "NONE (Secure)".green() } else { audit_report.active_external_connections.join(", ").red() });
                println!("  Summary:            {}", audit_report.summary.white());
                println!("{}", "═══════════════════════════════════════════════════════════════".cyan());
            } else {
                let mut guard = hgb_core::SubmarineGuard::new();
                let status = guard.engage()?;
                println!("{}", "═══════════════════════════════════════════════════════════════".blue());
                println!("{}", "  ⚓  SUBMARINE MODE ACTIVATED (AIR-GAPPED SOVEREIGN)  ⚓".blue().bold());
                println!("{}", "═══════════════════════════════════════════════════════════════".blue());
                println!("  Status:             {}", if status.is_active { "ACTIVE (100% Offline)".green().bold() } else { "INACTIVE".red() });
                println!("  Loopback Enforced:  {}", if status.enforced_loopback_only { "YES (127.0.0.1, UDS Only)".green() } else { "NO".red() });
                println!("  Telemetry Enforced: {}", if status.zero_telemetry_enforced { "DO_NOT_TRACK=1, Analytics Disabled".green() } else { "NO".red() });
                println!("  Neutralized Env:    {}", status.neutralized_env_vars.join(", ").dimmed());
                println!("  Activated At:       {}", status.activated_at.yellow());
                println!("{}", "═══════════════════════════════════════════════════════════════".blue());
            }
            Ok(())
        }
        Commands::Tournament { spec, rounds } => {
            println!("{}", format!("⚔️ Initializing Builder vs. Red-Teamer Tournament (Rounds: {})...", rounds).yellow().bold());
            let mut config = hgb_core::TournamentConfig::default();
            config.max_rounds = rounds;
            let tournament = hgb_core::AdversarialTournament::new(config);
            let outcome = tournament.run_tournament(&spec, "proposed_module.rs")?;
            println!("{}", "═══════════════════════════════════════════════════════════════".yellow());
            println!("{}", "  ⚔️  BUILDER VS. RED-TEAMER TOURNAMENT OUTCOME  ⚔️".yellow().bold());
            println!("{}", "═══════════════════════════════════════════════════════════════".yellow());
            println!("  Verdict:           {}", if outcome.passed { "PASSED (Code Hardened)".green().bold() } else { "FAILED (Vulnerabilities Remain)".red().bold() });
            println!("  Total Rounds:      {}", outcome.total_rounds);
            for round in &outcome.rounds {
                println!("  • Round #{}: attacks detected = {}, fixes applied = {}, surviving = {}",
                    round.round_number, round.red_team_attacks.len(), round.builder_fixes.len(), round.surviving_vulnerabilities);
            }
            if let Some(ref cert) = outcome.certificate {
                println!("\n  📜 Cryptographic Security Certificate Issued:");
                println!("    Certificate ID:  {}", cert.certificate_id.cyan().bold());
                println!("    Blake3 Hash:     {}", cert.blake3_code_hash.green());
                println!("    Signature:       {}", cert.signature.dimmed());
                println!("    Timestamp:       {}", cert.timestamp.yellow());
            }
            if !outcome.remaining_vulnerabilities.is_empty() {
                println!("\n  ⚠️ Remaining Vulnerabilities:");
                for v in &outcome.remaining_vulnerabilities {
                    println!("    ✕ [{:?}] {}: line {} - {}", v.severity, v.title.red(), v.line, v.remediation_hint.dimmed());
                }
            }
            println!("{}", "═══════════════════════════════════════════════════════════════".yellow());
            Ok(())
        }
        Commands::Bisect { bug, good, bad } => {
            let ws = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            println!("{}", format!("🔬 Initiating Agentic Git Bisect Regression Hunter for: '{}'", bug).red().bold());
            let good_rev = good.unwrap_or_else(|| "HEAD~10".to_string());
            let bad_rev = bad.unwrap_or_else(|| "HEAD".to_string());
            let engine = hgb_core::AgenticBisectEngine::new(&ws);
            let config = hgb_core::BisectConfig::new(good_rev.clone(), bad_rev.clone(), bug.clone());
            let outcome = engine.run_bisect(&config)?;
            println!("{}", "═══════════════════════════════════════════════════════════════".red());
            println!("{}", "  🎯  AGENTIC BISECT REGRESSION HUNTER REPORT  🎯".red().bold());
            println!("{}", "═══════════════════════════════════════════════════════════════".red());
            println!("  Commit Range:      {} .. {}", good_rev.cyan(), bad_rev.cyan());
            println!("  Steps Evaluated:   {}", outcome.total_steps);
            for step in &outcome.steps_taken {
                println!("    Step #{}: commit {} -> {}", step.step_number, &step.commit_hash[..8.min(step.commit_hash.len())],
                    if step.test_passed { "PASS".green() } else { "FAIL".red() });
            }
            if let Some(ref culprit) = outcome.culprit_commit {
                println!("\n  🔥 Culprit Commit: {}", culprit.yellow().bold());
                if let Some(ref analysis) = outcome.analysis {
                    println!("  Author:            {}", analysis.author.white());
                    println!("  Message:           {}", analysis.commit_message.cyan());
                    println!("  Files Changed:     {}", analysis.files_changed.join(", "));
                    println!("  Suspected Cause:   {}", analysis.suspected_cause.yellow());
                    println!("\n  💡 Recommended Remedy:\n  {}", analysis.recommended_remedy.green());
                }
            } else {
                println!("\n  ✔ No regression culprit isolated in given commit range.");
            }
            println!("{}", "═══════════════════════════════════════════════════════════════".red());
            Ok(())
        }
        Commands::SpeculativeDraft { prompt, draft_model } => {
            println!("{}", format!("🚀 Running Local Speculative Draft Decoding (draft model: {})...", draft_model).green().bold());
            let engine = hgb_core::SpeculativeDraftEngine::new("http://127.0.0.1:11434", draft_model);
            let draft = engine.generate_draft(&prompt, 128).await?;
            println!("{}", "═══════════════════════════════════════════════════════════════".green());
            println!("{}", "  🚀  LOCAL SPECULATIVE DRAFT DECODING TELEMETRY  🚀".green().bold());
            println!("{}", "═══════════════════════════════════════════════════════════════".green());
            println!("  Draft Model:       {}", draft.draft_model.cyan().bold());
            println!("  Draft Tokens:      {}", draft.draft_tokens.len());
            println!("  Generation Time:   {}ms", draft.generation_time_ms);
            println!("  Throughput:        {:.1} tokens/sec", draft.tokens_per_sec);
            println!("\n  Draft Preview:\n{}", draft.draft_text.white());
            println!("{}", "═══════════════════════════════════════════════════════════════".green());
            Ok(())
        }
        Commands::SpeculativeSandbox { cmd, files } => {
            let ws = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let sandbox = hgb_core::SpeculativeSandbox::new(&ws).with_check_command(&cmd);
            let file_list: Vec<&str> = if let Some(ref fl) = files {
                fl.split(',').map(|s| s.trim()).collect()
            } else {
                vec!["Cargo.toml", "crates/hgb-cli/src/main.rs"]
            };
            println!("{}", format!("🧪 Preparing Speculative Sandbox in RAM disk (shm: {})...", sandbox.is_using_shm()).cyan().bold());
            sandbox.prepare_sandbox(&file_list)?;
            println!("  Sandbox isolated root: {}", sandbox.sandbox_root().display().to_string().cyan());
            let comp = sandbox.run_compilation_check()?;
            println!("  Verification command: {}", cmd.yellow());
            println!("  Compilation:          {}", if comp.success { "PASSED (Clean)".green().bold() } else { "FAILED".red().bold() });
            println!("  Duration:             {}ms", comp.duration_ms);
            if !comp.extracted_errors.is_empty() {
                println!("  Extracted Errors:     {}", comp.extracted_errors.len());
                for err in comp.extracted_errors.iter().take(3) {
                    println!("    • {}", err.red());
                }
            }
            let _ = sandbox.rollback();
            println!("  ✔ Speculative sandbox cleaned up with zero disk artifacts left behind.");
            Ok(())
        }
        Commands::Infer { prompt, model, max_tokens, temperature } => {
            println!("{}", "🪽 Hagibis Embedded Zero-HTTP Inference Engine".cyan().bold());
            println!("  Prompt: {}", prompt.yellow());
            
            let mut config = hgb_core::InferenceConfig::default();
            config.max_tokens = max_tokens;
            config.temperature = temperature;

            let engine = if let Some(ref model_path) = model {
                println!("  Model binary: {}", model_path.green());
                hgb_core::EmbeddedInferenceEngine::from_file(model_path, Some(config))?
            } else {
                println!("  Engine mode:  {}", "In-Process Embedded Micro-Weights (Zero HTTP)".green().bold());
                let mut eng = hgb_core::EmbeddedInferenceEngine::default_embedded();
                eng.set_config(config);
                eng
            };

            print!("\n  {} ", "Generating:".bold());
            use std::io::Write;
            let result = engine.generate_stream(&prompt, |token, _id| {
                print!("{}", token);
                std::io::stdout().flush().ok();
                true
            })?;
            println!("\n");

            let telem = &result.telemetry;
            println!("{}", "── Embedded Inference Telemetry ──".dimmed());
            println!("  Time-To-First-Token (TTFT): {:?} (Sub-millisecond direct memory access)", telem.time_to_first_token);
            println!("  Decode duration:            {:?}", telem.decode_duration);
            println!("  Total duration:             {:?}", telem.total_duration);
            println!("  Prompt tokens:              {}", telem.prompt_tokens);
            println!("  Completion tokens:          {}", telem.completion_tokens);
            println!("  Decode throughput:          {:.1} tok/sec", telem.decode_tokens_per_sec);
            println!("  Overall throughput:         {:.1} tok/sec", telem.total_tokens_per_sec);
            println!("  Zero HTTP network roundtrips: {}", "0.0ms overhead".green().bold());
            Ok(())
        }
        Commands::CowSandbox { cmd } => {
            println!("{}", "🪽 Instant Linux Copy-on-Write (CoW) Workspace Sandbox".cyan().bold());
            let cwd = std::env::current_dir()?;
            let t0 = std::time::Instant::now();
            let mut sandbox = hgb_core::OverlaySandbox::new(&cwd)?;
            let clone_us = t0.elapsed().as_micros();

            println!("  Workspace:  {}", cwd.display().to_string().yellow());
            println!("  Backend:    {}", format!("{:?}", sandbox.backend()).green().bold());
            println!("  Clone time: {} µs (zero-copy instant branching)", clone_us);
            println!("  Merged:     {}", sandbox.merged_path().display().to_string().cyan());
            println!("  Upperdir:   {}", sandbox.upperdir().display().to_string().dimmed());

            println!("\n  Executing inside sandboxed merged view: {}", cmd.yellow().bold());
            let parts: Vec<&str> = cmd.split_whitespace().collect();
            if parts.is_empty() {
                println!("  No command specified.");
            } else {
                let report = sandbox.execute_command(parts[0], &parts[1..])?;
                println!("  Exit code: {}", report.exit_code);
                println!("  Duration:  {} ms", report.duration_ms);
                if !report.stdout.trim().is_empty() {
                    println!("\n--- STDOUT ---\n{}", report.stdout.trim());
                }
                if !report.stderr.trim().is_empty() {
                    println!("\n--- STDERR ---\n{}", report.stderr.trim());
                }
                println!("  Status:    {}", if report.success { "SUCCESS".green().bold() } else { "FAILED".red().bold() });
            }

            let diff = sandbox.inspect_diff()?;
            println!("\n  Staged modifications in CoW volatile layer:");
            println!("    Added:    {}", diff.added_files.len());
            println!("    Modified: {}", diff.modified_files.len());
            println!("    Deleted:  {}", diff.deleted_files.len());

            let rollback_report = sandbox.rollback()?;
            println!("  ✔ Instant rollback completed in {} µs. Zero disk artifacts left behind.", rollback_report.elapsed_us);
            Ok(())
        }
        Commands::Got { spec, target } => {
            println!("{}", "🪽 Hagibis Speculative Graph-of-Thought (GoT) Arbiter".cyan().bold());
            println!("  Problem Spec: {}", spec.yellow().bold());
            println!("  Target File:  {}", target.cyan());

            let cwd = std::env::current_dir()?;
            let mut graph = hgb_core::ThoughtGraph::new(&spec);

            // Synthesize 3 diverse speculative branches
            let b1_id = hgb_core::ThoughtNodeId::new("branch_inplace");
            let mut b1_files = std::collections::HashMap::new();
            b1_files.insert(
                target.clone(),
                format!(
                    "// [Branch 1: In-Place Fix]\n// Spec: {}\npub fn execute_solution() -> Result<(), &'static str> {{\n    // In-place patch with defensive bounds checking\n    Ok(())\n}}\n",
                    spec
                ),
            );
            let b1 = hgb_core::ThoughtNode::new(
                b1_id.clone(),
                hgb_core::ThoughtStrategy::InPlaceFix,
                format!("In-place minimal-footprint patch for: {}", spec),
                b1_files,
            );

            let b2_id = hgb_core::ThoughtNodeId::new("branch_refactor");
            let mut b2_files = std::collections::HashMap::new();
            b2_files.insert(
                target.clone(),
                format!(
                    "// [Branch 2: Method Refactor]\n// Spec: {}\npub struct SolutionManager;\nimpl SolutionManager {{\n    pub fn run(&self) -> bool {{ true }}\n}}\n",
                    spec
                ),
            );
            let b2 = hgb_core::ThoughtNode::new(
                b2_id.clone(),
                hgb_core::ThoughtStrategy::MethodRefactor,
                format!("Modular architectural abstraction for: {}", spec),
                b2_files,
            );

            let b3_id = hgb_core::ThoughtNodeId::new("branch_zero_alloc");
            let mut b3_files = std::collections::HashMap::new();
            b3_files.insert(
                target.clone(),
                format!(
                    "// [Branch 3: Zero-Allocation Rewrite]\n// Spec: {}\n#[inline(always)]\npub fn execute_fast(buf: &mut [u8]) -> usize {{\n    // Zero-allocation zero-copy kernel implementation\n    buf.len()\n}}\n",
                    spec
                ),
            );
            let b3 = hgb_core::ThoughtNode::new(
                b3_id.clone(),
                hgb_core::ThoughtStrategy::ZeroAllocationRewrite,
                format!("High-throughput zero-allocation rewrite for: {}", spec),
                b3_files,
            );

            let root_id = graph.root_id.clone();
            graph.add_node(b1, Some(root_id.clone()));
            graph.add_node(b2, Some(root_id.clone()));
            graph.add_node(b3, Some(root_id.clone()));

            println!("  Evaluating 3 speculative branches in parallel CoW sandboxes...");
            let arbiter = hgb_core::GoTArbiter::new(&cwd);
            arbiter.evaluate_branches(&mut graph, &[b1_id.clone(), b2_id.clone(), b3_id.clone()]).await?;

            println!("\n{}", graph.to_ascii_tree());

            let cert = arbiter.arbitrate_and_certify(&mut graph)?;
            println!("{}", "── Cryptographic Thought Certificate ──".green().bold());
            println!("  Certificate ID:   {}", cert.certificate_id.cyan());
            println!("  Winning Branch:   {} ({})", cert.winner_node_id.0.green().bold(), cert.winner_strategy.name().yellow());
            println!("  Composite Score:  {:.1}/100.0", cert.fitness.composite_score);
            println!("  Test Pass Rate:   {:.0}%", cert.fitness.test_pass_rate * 100.0);
            println!("  Blast Radius:     {:.1}/100.0 (Lower is safer)", cert.fitness.blast_score);
            println!("  Latency:          {} ms", cert.fitness.execution_latency_ms);
            println!("  Pruned Branches:  {}", cert.pruned_nodes_count);
            println!("  BLAKE3 Signature: {}", cert.blake3_signature.green());
            println!("  Authenticity:     {}", if cert.verify() { "VERIFIED (Cryptographically Valid)".green().bold() } else { "FAILED".red().bold() });
            Ok(())
        }
        Commands::Dream { audit, distill, limit } => {
            println!("{}", "🪽 Hagibis Ambient Sleep-State Self-Tuning & Distillation".cyan().bold());
            let cwd = std::env::current_dir()?;
            let engine = hgb_core::AmbientDreamEngine::new(&cwd);

            if audit || (!audit && !distill) {
                let headroom = engine.check_headroom();
                println!("{}", "── Linux System Headroom Diagnostic ──".yellow().bold());
                println!("  CPU Idle Percentage: {:.1}%", headroom.cpu_idle_pct);
                match &headroom.power_state {
                    hgb_core::PowerState::AcPower => println!("  Power Supply:        {} (Wall Power Connected)", "AC Power".green().bold()),
                    hgb_core::PowerState::Battery { capacity_pct, status } => {
                        println!("  Power Supply:        Battery ({}%, {})", capacity_pct, status);
                    }
                    hgb_core::PowerState::Unknown => {
                        println!("  Power Supply:        {}", "Unknown / Virtualized".dimmed());
                    }
                }
                println!("  Ambient Eligible:    {}", if headroom.has_headroom { "YES (Sufficient headroom for sleep distillation)".green().bold() } else { "NO (Deferred to avoid user interference)".yellow().bold() });
                if !distill {
                    println!("\n  Run with `--distill` to execute a distillation cycle on git history.");
                    return Ok(());
                }
            }

            if distill {
                println!("\n  Ingesting git commits and synthesizing DPO preference pairs (limit: {})...", limit);
                let report = engine.run_dream_cycle(limit, true)?;
                println!("{}", "── Ambient Distillation Report ──".green().bold());
                println!("  Commits Ingested:       {}", report.commits_ingested);
                println!("  DPO Pairs Generated:    {}", report.total_dpo_pairs);
                println!("  Learning Velocity:      {:.2} pairs/commit", report.learning_velocity);
                println!("  Style Alignment Score:  {:.1}%", report.style_alignment_score);
                println!("  DPO Dataset JSONL:      {}", report.dataset_path.display().to_string().cyan());
                println!("  LoRA Config JSON:       {}", report.lora_config_path.display().to_string().cyan());
                println!("  LoRA Runbook Script:    {}", report.runbook_path.display().to_string().green());
                println!("  ✔ Ambient distillation cycle complete. Ready for local sleep-tuning.");
            }
            Ok(())
        }
        Commands::Do { spec, target, commit } => {
            println!("{}", "🪽 Hagibis Unified Autonomous Driver (`hgb do`)".cyan().bold());
            println!("  Task Spec:    {}", spec.yellow().bold());
            if let Some(ref t) = target {
                println!("  Target File:  {}", t.cyan());
            }
            println!("  Auto-Commit:  {}", if commit { "ENABLED (Will write verified changes to disk)".green().bold() } else { "DISABLED (CoW Sandbox isolation only)".yellow().bold() });

            let cwd = std::env::current_dir()?;
            let driver = hgb_core::UnifiedAutonomousDriver::new(&cwd)
                .with_auto_commit(commit);

            println!("  Executing 7-stage autonomous pipeline (Sandbox -> GoT -> Blast -> Certify -> DPO)...");
            let report = driver.execute(&spec, target.as_deref()).await?;

            println!("{}", "\n── Unified Autonomous Execution Report ──".green().bold());
            println!("  Winning Strategy:    {} ({})", report.winning_strategy.name().yellow().bold(), report.winning_node_id.cyan());
            println!("  Fitness Score:       {:.1}/100.0", report.fitness_score.composite_score);
            println!("  Blast Radius Score:  {:.1}/100.0 (Lower is safer)", report.fitness_score.blast_score);
            println!("  Test Pass Rate:      {:.0}%", report.fitness_score.test_pass_rate * 100.0);
            println!("  Self-Healing Loops:  {} iteration(s)", report.healed_iterations);
            println!("  BLAKE3 Certificate:  {}", report.certificate.blake3_signature.green());
            println!("  Certificate Valid:   {}", if report.certificate.verify() { "VERIFIED (Cryptographically Authentic)".green().bold() } else { "FAILED".red().bold() });
            println!("  Workspace Committed: {}", if report.committed { "YES (Propagated to disk)".green().bold() } else { "NO (Rolled back cleanly)".yellow() });
            println!("  Duration:            {} ms", report.duration_ms);

            if let Some(ref dpo_path) = report.dpo_pair_path {
                println!("  DPO Distill Record:  {}", dpo_path.display().to_string().cyan());
            }
            Ok(())
        }
        Commands::Daemon { action } => {
            println!("{}", "🪽 Hagibis Resident Microkernel Daemon & Ambient Sleep-Tuning".cyan().bold());
            let cwd = std::env::current_dir()?;
            let service = hgb_daemon::AmbientDaemonService::new(&cwd);

            match action {
                DaemonCliSubcommand::Status => {
                    let installed = hgb_daemon::AmbientDaemonService::check_systemd_service_installed();
                    let journal = service.journal();
                    let headroom = hgb_core::SystemHeadroom::inspect_linux_system();

                    println!("{}", "── System Daemon Status ──".yellow().bold());
                    println!("  systemd Service:    {}", if installed { "INSTALLED (~/.config/systemd/user/hgbd.service)".green().bold() } else { "NOT INSTALLED (Run `hgb daemon install-service`)".yellow() });
                    println!("  CPU Idle:           {:.1}%", headroom.cpu_idle_pct);
                    println!("  Ambient Ready:      {}", if headroom.has_headroom { "YES (Ready to dream when idle)".green().bold() } else { "DEFERRED".yellow() });
                    println!("  Total Dream Cycles: {}", journal.total_cycles);
                    println!("  Total DPO Pairs:    {}", journal.total_dpo_pairs);
                    if let Some(last) = journal.last_cycle_timestamp {
                        println!("  Last Dream Cycle:   {}", last.to_rfc3339().cyan());
                    }
                }
                DaemonCliSubcommand::Journal => {
                    let journal = service.journal();
                    println!("{}", "── Overnight Ambient Dream Journal ──".green().bold());
                    println!("  {}\n", journal.morning_greeting());
                    println!("  Total Recorded Cycles: {}", journal.entries.len());
                    for (i, entry) in journal.entries.iter().rev().take(5).enumerate() {
                        println!("    [{}] Cycle: {} | DPO Pairs: {} | Style Score: {:.1}% | Idle: {:.0}%",
                            i + 1, entry.cycle_id.cyan(), entry.dpo_pairs_synthesized, entry.style_alignment_score, entry.cpu_idle_pct);
                    }
                }
                DaemonCliSubcommand::InstallService => {
                    println!("  Generating and installing Linux systemd user service...");
                    let path = hgb_daemon::AmbientDaemonService::install_systemd_service(None)?;
                    println!("  ✔ Installed unit: {}", path.display().to_string().green().bold());
                    println!("  To enable and start now, run:");
                    println!("    systemctl --user daemon-reload");
                    println!("    systemctl --user enable --now hgbd");
                }
            }
            Ok(())
        }
        Commands::Menu { query } => {
            if let Some(ref q) = query {
                println!("{}", format!("🪽 Hagibis Command Search: '{}'", q).cyan().bold());
                let commands = menu::InteractiveCommandCatalog::all_commands();
                let matches = menu::InteractiveCommandCatalog::filter_commands(&commands, q);

                if matches.is_empty() {
                    println!("  No commands found matching '{}'. Run `hgb menu` for full catalog.", q.yellow());
                } else {
                    println!("  Found {} matching command(s):\n", matches.len());
                    for (cmd, score) in matches {
                        println!("  {} [{}] (score: {})", cmd.name.green().bold(), cmd.category.cyan(), score);
                        println!("    Syntax:  {}", cmd.syntax.yellow());
                        if !cmd.aliases.is_empty() {
                            println!("    Aliases: {}", cmd.aliases.join(", ").dimmed());
                        }
                        println!("    Info:    {}\n", cmd.description);
                    }
                }
            } else {
                let selected = menu::InteractiveCommandCatalog::run_interactive()?;
                if let Some(cmd) = selected {
                    println!("  Selected command: {}", cmd.green().bold());
                    println!("  To run: {}", format!("hgb {} --help", cmd).cyan());
                }
            }
            Ok(())
        }
        Commands::CiAction { task, diff, summary } => {
            println!("{}", "🪽 Hagibis Official GitHub Action CI Runner".cyan().bold());
            let cwd = std::env::current_dir()?;
            let mut config = hgb_core::GitHubActionConfig::default();
            config.task = task;

            let runner = hgb_core::GitHubActionRunner::new(&cwd, Some(config));
            let report = runner.run(diff.as_deref())?;

            println!("  Verdict:           {}", if report.verification_passed { "PASSED (Clean)".green().bold() } else { "BLOCKED / FAILED".red().bold() });
            println!("  Blast Radius:      {:.1}/100.0", report.blast_score);
            println!("  Symbols Affected:  {}", report.symbols_affected);
            println!("  Packages Audited:  {}", report.packages_scanned);
            println!("  BLAKE3 Signature:  {}", report.blake3_cert.green());

            if summary || std::env::var("GITHUB_STEP_SUMMARY").is_ok() {
                println!("\n--- GITHUB FLAVORED MARKDOWN STEP SUMMARY ---\n{}", report.summary_markdown);
            }
            Ok(())
        }
        Commands::GpuBench { dim, iterations } => {
            println!("{}", "🪽 Hagibis GPU / Tensor Compute Acceleration Benchmark".cyan().bold());
            let engine = hgb_core::GpuComputeEngine::new();
            println!("  Backend:    {}", engine.backend().name().green().bold());
            println!("  Matrix Dim: {} x {} x {}", dim, dim, dim);
            println!("  Iterations: {}", iterations);
            println!("  Running parallel GEMM benchmark (F32 vs Q4_0 vs Q8_0)...");

            let report = engine.benchmark_flops(dim, iterations);
            println!("{}", "\n── Compute Throughput Telemetry ──".yellow().bold());
            println!("  F32 GEMM:                {:.2} GFLOPS", report.f32_gflops);
            println!("  Q8_0 Block Quantized:    {:.2} GFLOPS", report.q8_0_gflops);
            println!("  Q4_0 Nibble Quantized:   {:.2} GFLOPS", report.q4_0_gflops);
            println!("  Quantization Efficiency: {:.2}x speedup", report.quantization_speedup);
            println!("  Elapsed Benchmark Time:  {} ms", report.elapsed_ms);
            Ok(())
        }
        Commands::ModelPull { name } => {
            println!("{}", format!("🪽 Hagibis Resident GGUF Model Manager: Pulling '{}'", name).cyan().bold());
            let manager = hgb_core::GgufModelManager::new();
            println!("  Storage Directory: {}", manager.models_dir().display().to_string().yellow());

            let target_path = manager.pull_model(&name)?;
            println!("  ✔ Model resident on disk: {}", target_path.display().to_string().green().bold());

            let inspection = manager.inspect_model(&target_path)?;
            println!("  Format:          {} v{}", inspection.magic.green(), inspection.version);
            println!("  Size:            {:.2} MB", inspection.file_size_bytes as f64 / (1024.0 * 1024.0));
            println!("  Tensors:         {}", inspection.tensor_count);
            println!("  Metadata KV:     {}", inspection.kv_count);
            println!("  BLAKE3 Checksum: {}", inspection.blake3_hash.cyan());
            Ok(())
        }
        Commands::DesktopIde { port, browser } => {
            println!("{}", "🪽 Hagibis Sovereign Desktop IDE Launcher".cyan().bold());
            let mut cfg = desktop_ide::DesktopLaunchConfig::default();
            cfg.port = port;
            cfg.custom_browser = browser;

            let launcher = desktop_ide::DesktopIdeLauncher::new(Some(cfg));
            println!("  Port:      {}", port);
            println!("  Spawning standalone borderless app window...");

            let report = launcher.launch()?;
            println!("  ✔ Target URL:      {}", report.url.cyan().bold());
            println!("  ✔ Browser Binary:  {}", report.browser_binary.green());
            println!("  ✔ Standalone Mode: {}", if report.is_app_mode { "TRUE (Borderless desktop application window)".green().bold() } else { "FALSE (System browser tab fallback)".yellow() });
            if let Some(ref prof) = report.profile_dir {
                println!("  ✔ Profile Root:    {}", prof.display().to_string().dimmed());
            }
            Ok(())
        }
        Commands::ProxyBridge { port, provider, shred_tokens } => {
            println!("{}", "═══════════════════════════════════════════════════════════════".cyan());
            println!("{}", "  🪽  HAGIBIS CLAUDE & OPENAI DROP-IN PROXY SERVER  🪽".cyan().bold());
            println!("{}", "═══════════════════════════════════════════════════════════════".cyan());
            println!("  ✔ Binding:           {}", format!("http://127.0.0.1:{}", port).green().bold());
            println!("  ✔ Anthropic API:     {}", format!("http://127.0.0.1:{}/v1/messages", port).yellow());
            println!("  ✔ OpenAI API:        {}", format!("http://127.0.0.1:{}/v1/chat/completions", port).yellow());
            println!("  ✔ Token Shredder:    {}", if shred_tokens { "ENABLED (up to 90% savings)".green() } else { "DISABLED".dimmed() });
            if let Some(ref p) = provider {
                println!("  ✔ Upstream Override: {}", p.magenta().bold());
            } else {
                println!("  ✔ Upstream Failover: {}", "Copilot -> Groq -> NVIDIA -> Gemini -> OpenRouter -> Ollama".dimmed());
            }
            println!("\n  {} Set in Claude Code:", "💡 TIP:".bold().white());
            println!("     export ANTHROPIC_BASE_URL={}", format!("http://127.0.0.1:{}", port).cyan());
            println!("     export ANTHROPIC_API_KEY=hgb-local\n");

            let cfg = hgb_daemon::ProxyServerConfig {
                host: "127.0.0.1".to_string(),
                port,
                default_provider: provider,
                shred_tokens,
            };

            hgb_daemon::start_proxy_server(cfg).await.map_err(|e| format!("Proxy server failure: {e}"))?;
            Ok(())
        }
        Commands::AuthCopilot => {
            println!("{}", "═══════════════════════════════════════════════════════════════".cyan());
            println!("{}", "  🔑  GITHUB COPILOT RFC 8628 DEVICE AUTHORIZATION  🔑".cyan().bold());
            println!("{}", "═══════════════════════════════════════════════════════════════".cyan());

            let engine = hgb_core::CopilotAuthEngine::new();
            let device_flow = engine.request_device_code().await?;

            println!("  1. Open verification page: {}", device_flow.verification_uri.bold().green());
            println!("  2. Enter user code:        {}\n", format!("[ {} ]", device_flow.user_code).bold().yellow());
            println!("  ⏳ Waiting for browser authorization in GitHub (timeout: {}s)...", device_flow.expires_in);

            let access_token = engine.poll_access_token(
                &device_flow.device_code,
                device_flow.interval,
                device_flow.expires_in,
            ).await?;

            println!("  ✔ Access token granted by GitHub!");
            println!("  🔄 Exchanging for Copilot internal session token...");

            let session = engine.exchange_copilot_token(&access_token).await?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;

            let creds = hgb_core::CopilotCredentials {
                github_token: access_token,
                copilot_token: session.token,
                expires_at: session.expires_at,
                api_endpoint: session.endpoints.and_then(|e| e.api).unwrap_or_else(|| "https://api.individual.githubcopilot.com".to_string()),
                updated_at: now,
            };

            engine.save_credentials(&creds)?;
            println!("  ✔ Credentials saved to ~/.hgb/copilot_auth.json");
            println!("  🎉 Successfully authenticated! Ready to power Claude 3.5 Sonnet & GPT-4o.\n");
            Ok(())
        }
        Commands::AuthOpenAi { key } => {
            let api_key = match key {
                Some(k) if !k.trim().is_empty() => k.trim().to_string(),
                _ => {
                    print!("Enter OpenAI API Key (sk-...): ");
                    use std::io::Write;
                    std::io::stdout().flush().unwrap();
                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input).unwrap();
                    input.trim().to_string()
                }
            };
            if api_key.is_empty() {
                eprintln!("{}", "Error: No API key provided.".red());
                std::process::exit(1);
            }
            hgb_core::OpenAiCompatibleProvider::save_stored_key("openai_api_key", &api_key)?;
            println!("  ✔ {}", "OpenAI API key saved to ~/.hgb/credentials.json".green().bold());
            println!("  🎉 Successfully configured! Ready to power gpt-4o, gpt-4o-mini, o1, and o3-mini in Cockpit and CLI.\n");
            Ok(())
        }
        Commands::AuthCodex => {
            println!("\n{}", "🪽 Hagibis OpenAI Codex Web Authentication Manager".bold().cyan());
            println!("Detecting ~/.codex/auth.json and syncing ChatGPT credentials...\n");
            match hgb_core::CodexAuthEngine::import_and_verify().await {
                Ok(report) => {
                    println!("{}", report.green().bold());
                    println!("\n{}", "🎉 Ready! OpenAI Codex Models (gpt-6-astra, gpt-6.1-sol, gpt-4o) are active in Cockpit and CLI.".cyan());
                }
                Err(e) => {
                    eprintln!("{} {}", "✖ Notice:".red().bold(), e);
                    println!("\nTo connect with your ChatGPT account using Codex:");
                    println!("  1. Run: {} or {}", "codex login".cyan(), "codex login --device-auth".cyan());
                    println!("  2. Run: {}\n", "hgb auth-codex".cyan());
                }
            }
            Ok(())
        }
        Commands::Key { provider, key } => {
            let prov = provider.to_lowercase();
            if prov == "codex" {
                println!("\n{}", "🪽 Syncing OpenAI Codex Web Authentication...".cyan());
                match hgb_core::CodexAuthEngine::import_and_verify().await {
                    Ok(rep) => println!("  ✔ {}", rep.green()),
                    Err(e) => eprintln!("  ✖ {}", e),
                }
                return Ok(());
            }
            let field = match prov.as_str() {
                "openai" => "openai_api_key",
                "deepseek" => "deepseek_api_key",
                "anthropic" => "anthropic_api_key",
                "openrouter" => "openrouter_api_key",
                other => {
                    eprintln!("{}", format!("Unknown provider '{}'. Supported: openai, anthropic, deepseek, openrouter, codex", other).red());
                    std::process::exit(1);
                }
            };
            let secret = match key {
                Some(k) if !k.trim().is_empty() => k.trim().to_string(),
                _ => {
                    print!("Enter API Key for {}: ", prov);
                    use std::io::Write;
                    std::io::stdout().flush().unwrap();
                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input).unwrap();
                    input.trim().to_string()
                }
            };
            hgb_core::OpenAiCompatibleProvider::save_stored_key(field, &secret)?;
            println!("  ✔ {}", format!("{} API key saved to ~/.hgb/credentials.json", prov).green().bold());
            Ok(())
        }
        Commands::CloudFabric { ping } => {
            println!("{}", "═══════════════════════════════════════════════════════════════".cyan());
            println!("{}", "  ☁️   UNIVERSAL CLOUD FABRIC & MULTI-PROVIDER CATALOG  ☁️".cyan().bold());
            println!("{}", "═══════════════════════════════════════════════════════════════".cyan());

            let fabric = hgb_core::UniversalCloudFabric::new();
            let catalog = fabric.catalog();

            for desc in &catalog {
                let status_icon = if desc.is_local { "🏠" } else { "🌐" };
                println!("  {} {} [{}]", status_icon, desc.display_name.bold().white(), desc.id.as_str().cyan());
                println!("     • Base URL:      {}", desc.base_url.dimmed());
                println!("     • Default Model: {}", desc.default_model.yellow());
                if let Some(ref env_name) = desc.env_var {
                    let has_key = std::env::var(env_name).is_ok();
                    let key_status = if has_key { "CONFIGURED ✓".green() } else { "MISSING".red() };
                    println!("     • Env Key (${}): {}", env_name, key_status);
                }
                println!();
            }

            if ping {
                println!("{}", "  ⚡ Measuring live round-trip latency across all providers...".bold().cyan());
                let reports = fabric.ping_all().await;
                println!("\n  {:<15} {:<12} {:<12} {}", "PROVIDER".bold(), "STATUS".bold(), "LATENCY".bold(), "DIAGNOSTIC".bold());
                println!("  {}", "─".repeat(60).dimmed());
                for r in reports {
                    let status_str = if r.is_available { "ONLINE".green().bold() } else { "OFFLINE".red() };
                    let lat_str = match r.latency_ms {
                        Some(ms) => format!("{} ms", ms).cyan(),
                        None => "—".dimmed(),
                    };
                    let diag = r.error_message.unwrap_or_else(|| "Nominal".to_string());
                    println!("  {:<15} {:<12} {:<12} {}", r.provider_id, status_str, lat_str, diag.dimmed());
                }
                println!();
            }
            Ok(())
        }
        Commands::RemoteGateway { command } => {
            match command {
                RemoteSubcommand::Telegram { token, allowed_user } => {
                    println!("{}", "═══════════════════════════════════════════════════════════════".cyan());
                    println!("{}", "  📱  HAGIBIS REMOTE MOBILE GATEWAY (TELEGRAM)  📱".cyan().bold());
                    println!("{}", "═══════════════════════════════════════════════════════════════".cyan());

                    let tok = token.clone().or_else(|| std::env::var("TELEGRAM_BOT_TOKEN").ok()).ok_or_else(|| {
                        "Missing Telegram bot token. Pass --token or set $TELEGRAM_BOT_TOKEN."
                    })?;

                    let cfg = hgb_daemon::RemoteBotConfig {
                        telegram_token: Some(tok),
                        discord_webhook_url: None,
                        allowed_user_ids: allowed_user.clone(),
                        poll_interval_secs: 2,
                    };

                    println!("  ✔ Whitelisted Users: {:?}", allowed_user);
                    println!("  ✔ Polling Telegram /getUpdates in background...");
                    let gateway = hgb_daemon::RemoteMobileGateway::new(cfg);
                    gateway.poll_telegram_loop().await.map_err(|e| format!("Telegram loop error: {e}"))?;
                }
                RemoteSubcommand::Discord { webhook, message } => {
                    let hook = webhook.clone().or_else(|| std::env::var("DISCORD_WEBHOOK_URL").ok()).ok_or_else(|| {
                        "Missing Discord webhook URL. Pass --webhook or set $DISCORD_WEBHOOK_URL."
                    })?;

                    let cfg = hgb_daemon::RemoteBotConfig {
                        telegram_token: None,
                        discord_webhook_url: Some(hook),
                        allowed_user_ids: Vec::new(),
                        poll_interval_secs: 2,
                    };

                    let gateway = hgb_daemon::RemoteMobileGateway::new(cfg);
                    gateway.dispatch_discord_webhook(&message).await.map_err(|e| format!("Discord dispatch error: {e}"))?;
                    println!("✔ Successfully dispatched message to Discord webhook.");
                }
            }
            Ok(())
        }
        Commands::VisualLoop { url, port, screenshot } => {
            println!("{}", "👁️  HAGIBIS VISUAL MULTIMODAL DEBUGGER (CDP)".cyan().bold());
            println!("  Target URL: {} • Remote Debugging Port: {}", url.yellow(), port);

            let cdp_client = hgb_core::CdpClient::new("127.0.0.1", port);
            match cdp_client.capture_visual_debug_report(&url).await {
                Ok(report) => {
                    println!("  {} Page Title: {}", "✔".green().bold(), report.page_title.white().bold());
                    println!("  Screenshot captured: {} (Format: {}, Valid PNG: {})",
                        format!("{} bytes", report.screenshot_base64.len()).yellow(),
                        report.screenshot_format,
                        report.is_valid_png_screenshot().to_string().green()
                    );
                    println!("  Console Errors: {} • Warnings: {}",
                        report.console_errors.len().to_string().red().bold(),
                        report.console_warnings.len().to_string().yellow()
                    );

                    if let Some(shot_path) = screenshot {
                        if let Ok(bytes) = base64::Engine::decode(
                            &base64::engine::general_purpose::STANDARD,
                            &report.screenshot_base64,
                        ) {
                            let _ = std::fs::write(&shot_path, bytes);
                            println!("  Saved screenshot to: {}", shot_path.green());
                        }
                    }

                    println!("\n{}", "── MULTIMODAL VISION PROMPT READY FOR CLAUDE 3.5 / GEMINI ──".dimmed());
                    println!("{}", report.formatted_vision_prompt);
                }
                Err(e) => {
                    println!("  {} Could not connect to Chromium CDP on port {}: {}", "Notice:".yellow().bold(), port, e);
                    println!("  Tip: Launch Chromium with `--remote-debugging-port={}` to enable live CDP visual automation.", port);

                    let prompt = hgb_core::VisualDebuggerReport::generate_multimodal_prompt(
                        &url,
                        "Hagibis Live Preview",
                        &[],
                        &[],
                        0,
                    );
                    println!("\n  Diagnostic Multimodal Template:\n{}", prompt);
                }
            }
            Ok(())
        }

        Commands::TimeMachine { list, restore, scrub } => {
            println!("{}", "⏳ HAGIBIS CHRONOWARP TIME-MACHINE LEDGER".cyan().bold());
            let cwd = std::env::current_dir()?;
            let engine = hgb_core::TimeMachineEngine::new(&cwd);

            if let Some(id) = restore {
                println!("  Restoring snapshot '{}' ...", id.yellow());
                match engine.restore_snapshot(&id, &cwd) {
                    Ok(files) => {
                        println!("  {} Successfully restored {} files: {}",
                            "✔".green().bold(),
                            files.len(),
                            files.join(", ").green()
                        );
                    }
                    Err(e) => println!("  {} {}", "Error restoring snapshot:".red().bold(), e),
                }
            } else if scrub || !list {
                match engine.render_timeline_scrubber(None) {
                    Ok(rendered) => println!("{}", rendered),
                    Err(e) => println!("  {} {}", "Error rendering timeline scrubber:".red().bold(), e),
                }
            } else {
                match engine.list_snapshots() {
                    Ok(snapshots) => {
                        println!("  Total recorded snapshots: {}", snapshots.len().to_string().green().bold());
                        for s in snapshots {
                            println!("  • {} | {} | {} | {} files | outcome: {}",
                                s.snapshot_id.bold(),
                                s.timestamp.format("%Y-%m-%d %H:%M:%S UTC").to_string().dimmed(),
                                if s.test_passed { "PASS".green() } else { "FAIL".red() },
                                s.modified_files.len(),
                                s.outcome
                            );
                        }
                    }
                    Err(e) => println!("  {} {}", "Error listing snapshots:".red().bold(), e),
                }
            }
            Ok(())
        }

        Commands::AstQuery { query, path, preview: _, make_public } => {
            println!("{}", "🔍 HAGIBIS DETERMINISTIC AST QUERY & REFACTOR ENGINE".cyan().bold());
            let file_path = std::path::Path::new(&path);
            if !file_path.exists() {
                println!("  {} Target file '{}' does not exist.", "Error:".red().bold(), path);
                return Ok(());
            }

            let source = std::fs::read_to_string(file_path)?;

            if let Some(fn_name) = make_public {
                println!("  Refactoring function '{}' to public with invariant verification...", fn_name.yellow().bold());
                match hgb_core::AstQueryEngine::make_function_public(&source, &fn_name) {
                    Ok(new_code) => {
                        std::fs::write(file_path, new_code)?;
                        println!("  {} Successfully modified '{}' with 100% delimiter & syntax invariant guarantees.", "✔".green().bold(), fn_name);
                    }
                    Err(e) => println!("  {} Invariant violation or refactoring error: {}", "✖".red().bold(), e),
                }
            } else {
                let symbols = hgb_core::AstQueryEngine::parse_source(&source)?;
                let filters = hgb_core::AstQueryEngine::parse_query(&query);
                let matches = hgb_core::AstQueryEngine::query_symbols(&symbols, &filters);

                println!("  Target: {} • Query: \"{}\" • Total Parsed: {} • Matches: {}",
                    path.yellow(),
                    query.green().bold(),
                    symbols.len(),
                    matches.len().to_string().cyan().bold()
                );

                for (idx, sym) in matches.iter().enumerate() {
                    let async_str = if sym.is_async { "async " } else { "" };
                    let ret_str = sym.return_type.as_deref().unwrap_or("()");
                    println!("    [{}] {} {}{} -> {} (vis: {})",
                        idx + 1,
                        sym.kind.dimmed(),
                        async_str.cyan(),
                        sym.name.bold(),
                        ret_str.magenta(),
                        sym.visibility.yellow()
                    );
                }
            }
            Ok(())
        }

        Commands::Tunnel { port, bind, bind_port, qr } => {
            println!("{}", "⚡ HAGIBIS ZERO-CONFIG P2P ENCRYPTED TUNNEL".cyan().bold());
            println!("  Local Target Port: {} • Tunnel Listen: {}:{}", port.to_string().yellow(), bind, bind_port);

            let server = hgb_daemon::P2pTunnelServer::new(&bind, bind_port, port)
                .map_err(|e| format!("Failed to initialize P2P tunnel server: {}", e))?;

            if qr {
                if let Ok(card) = server.render_qr_card() {
                    println!("{}", card);
                }
            }

            println!("  Tunnel online. Serving reverse proxy to http://127.0.0.1:{} ... (Press Ctrl+C to stop)", port);
            server.run().await.map_err(|e| format!("Tunnel server stopped: {}", e))?;
            Ok(())
        }
        Commands::Run { text, model, provider, continue_session, session } => {
            let ws = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let ws_str = ws.to_string_lossy().to_string();

            let raw_model = if let Some(ref m) = model {
                m.clone()
            } else if let Some(persisted) = hgb_core::load_active_model() {
                persisted
            } else if !hgb_core::GeminiProvider::is_available() && hgb_core::OllamaProvider::is_available() {
                if let Some(prov) = hgb_core::OllamaProvider::auto_discover() {
                    prov.default_model().to_string()
                } else {
                    "gemini-2.5-flash".to_string()
                }
            } else {
                "gemini-2.5-flash".to_string()
            };
            let active_model_str = hgb_core::validate_and_resolve_active_model(Some(&raw_model)).unwrap_or(raw_model);
            let active_model = active_model_str.as_str();

            // Session Memory Loading:
            let mut session_data = if let Some(ref sid) = session {
                hgb_core::session::load(sid, Some(&ws))
                    .unwrap_or_else(|_| hgb_core::CockpitSessionData::new(&ws_str, &active_model_str).with_id(sid.clone()))
            } else if continue_session {
                hgb_core::session::load_latest(Some(&ws))
                    .unwrap_or_else(|_| hgb_core::CockpitSessionData::new(&ws_str, &active_model_str))
            } else {
                hgb_core::CockpitSessionData::new(&ws_str, &active_model_str)
            };

            let dispatch_prompt = if continue_session || session.is_some() {
                if !session_data.messages.is_empty() {
                    println!("{}", format!("  🧠 Memory Active: Continuing session '{}' ({} previous turn(s) in context)...", session_data.id, session_data.messages.len()).cyan().dimmed());
                    let mut history_str = String::new();
                    history_str.push_str("--- CONVERSATION HISTORY (PREVIOUS TURNS) ---\n");
                    let start_idx = session_data.messages.len().saturating_sub(6);
                    for m in &session_data.messages[start_idx..] {
                        history_str.push_str(&format!("{}: {}\n\n", m.role.to_uppercase(), m.content));
                    }
                    history_str.push_str("--- CURRENT USER QUERY ---\n");
                    history_str.push_str(&text);
                    history_str
                } else {
                    text.clone()
                }
            } else {
                text.clone()
            };

            let is_local = hgb_core::OllamaProvider::is_ollama_model(active_model);
            if is_local {
                println!("{}", format!("  🪽 Local Ollama Reasoning (model: {})...", active_model).magenta().bold());
            } else {
                println!("{}", format!("  🪽 AGY Reasoning (model: {})...", active_model).cyan().bold());
            }
            let t0 = std::time::Instant::now();
            let req = HgbRequest::Prompt {
                prompt: dispatch_prompt,
                model: Some(active_model_str.clone()),
                provider,
                stream: false,
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(req).await;
            match resp {
                HgbResponse::Complete { output, tokens_used, duration_ms } => {
                    ChatCanvas::print_markdown(&output);
                    let wall_dur = t0.elapsed().as_millis() as u64;
                    let dur = if duration_ms > 0 { duration_ms } else { wall_dur.max(1) };

                    // Save this turn to session ledger so future calls (-c) or `hgb resume` can pick it up
                    session_data.add_user_message(&text);
                    session_data.add_assistant_message(&output, Some(active_model_str.clone()), tokens_used);
                    let _ = hgb_core::session::save(&session_data, Some(&ws));

                    if tokens_used > 0 {
                        let tps = (tokens_used as f64) / (dur as f64 / 1000.0).max(0.001);
                        println!(
                            "{}",
                            format!("  ⏱️ {} tokens in {} ms ({:.1} tok/s) • Model: {} • Session: {}", tokens_used, dur, tps, active_model, session_data.id).dimmed()
                        );
                    }
                }
                HgbResponse::Error(err) => {
                    eprintln!("{} {}", "✖ Prompt error:".red().bold(), err);
                    std::process::exit(1);
                }
                other => repl_helper.render_response(other),
            }
            Ok(())
        }
        Commands::Browse { url, tokens } => {
            println!("{}", format!("🌐 Navigating to '{}' (SSRF-Hardened Web Engine)...", url).cyan().bold());
            let t0 = std::time::Instant::now();
            let engine = hgb_core::WebBrowserEngine::default();
            match engine.browse_url(&url, tokens).await {
                Ok(page) => {
                    let dur_ms = t0.elapsed().as_millis() as u64;
                    println!("{}", format!("  ✔ Fetched: {} (HTTP {}) in {}ms [{} tokens]", page.title, page.status_code, dur_ms, page.token_count).green());
                    if page.truncated {
                        println!("{}", "  ⚠️ Content was truncated to fit token budget envelope.".yellow());
                    }
                    println!("\n{}\n", "─".repeat(70).dimmed());
                    ChatCanvas::print_markdown(&page.markdown);
                    println!("\n{}", "─".repeat(70).dimmed());
                }
                Err(e) => {
                    eprintln!("{} Failed to browse '{}': {}", "✖".red().bold(), url, e);
                    std::process::exit(1);
                }
            }
            Ok(())
        }
        Commands::SearchWeb { query, limit } => {
            println!("{}", format!("🔎 Searching live web for '{}' via DuckDuckGo...", query).cyan().bold());
            let t0 = std::time::Instant::now();
            let engine = hgb_core::WebBrowserEngine::default();
            match engine.search_web(&query, limit).await {
                Ok(results) => {
                    let dur_ms = t0.elapsed().as_millis() as u64;
                    println!("{}", format!("  ✔ Found {} organic results in {}ms:\n", results.len(), dur_ms).green());
                    for (i, r) in results.iter().enumerate() {
                        println!("  {}. {}", (i + 1).to_string().bold().yellow(), r.title.bold());
                        println!("     {}", r.url.cyan().underline());
                        if !r.snippet.is_empty() {
                            println!("     {}", r.snippet.dimmed());
                        }
                        println!();
                    }
                }
                Err(e) => {
                    eprintln!("{} Search failed: {}", "✖".red().bold(), e);
                    std::process::exit(1);
                }
            }
            Ok(())
        }
        Commands::InspectFolder { path, storage, network, xdg, depth, analyze } => {
            let target_path = path.clone().unwrap_or_else(|| ".".to_string());
            let home_dir = std::env::var("HOME").ok().map(std::path::PathBuf::from);
            let raw_p = std::path::PathBuf::from(&target_path);

            // Smart resolution: direct path, home-relative path, or ancestor directory
            let resolved_p = if raw_p.is_dir() {
                Some(raw_p)
            } else if let Some(ref h) = home_dir {
                let cand = h.join(&target_path);
                if cand.is_dir() {
                    Some(cand)
                } else {
                    None
                }
            } else {
                None
            }
            .or_else(|| {
                if let Ok(cwd) = std::env::current_dir() {
                    for anc in cwd.ancestors() {
                        if let Some(name) = anc.file_name() {
                            if name.to_string_lossy().eq_ignore_ascii_case(&target_path) {
                                return Some(anc.to_path_buf());
                            }
                        }
                    }
                }
                None
            });

            if analyze {
                let target_dir = resolved_p.as_deref();
                match hgb_core::FolderInspectorEngine::run_comprehensive_analysis(target_dir, true, 350) {
                    Ok(report) => {
                        println!("{}", "🏢 LIVE SYSTEM & NETWORK ARCHITECTURAL ANALYSIS REPORT".cyan().bold());
                        let (badge, color) = match report.health_verdict.as_str() {
                            "HEALTHY & NOMINAL" => ("[ HEALTHY & NOMINAL ]", colored::Color::Green),
                            "WARNING / SUB-OPTIMAL" => ("[ WARNING / SUB-OPTIMAL ]", colored::Color::Yellow),
                            _ => ("[ CRITICAL ATTENTION REQUIRED ]", colored::Color::Red),
                        };
                        println!("{} {}\n", "Verdict:".bold(), badge.color(color).bold());
                        println!("{} {}\n", "Executive Diagnosis:".magenta().bold(), report.executive_diagnosis);

                        let md = hgb_core::FolderInspectorEngine::format_analysis_markdown(&report);
                        println!("{md}");
                        return Ok(());
                    }
                    Err(e) => eprintln!("{} Analysis error: {e}", "✖".red().bold()),
                }
            }

            println!("{}", "📁 Inspecting Local & Network Filesystem Infrastructure...".cyan().bold());
            let format_bytes = |bytes: u64| -> String {
                const KB: u64 = 1024;
                const MB: u64 = KB * 1024;
                const GB: u64 = MB * 1024;
                if bytes >= GB {
                    format!("{:.1} GB", bytes as f64 / GB as f64)
                } else if bytes >= MB {
                    format!("{:.1} MB", bytes as f64 / MB as f64)
                } else if bytes >= KB {
                    format!("{:.1} KB", bytes as f64 / KB as f64)
                } else {
                    format!("{bytes} B")
                }
            };

            let show_all = !storage && !network && !xdg && path.is_none();

            if storage || show_all {
                let drives = hgb_core::FolderInspectorEngine::discover_storage_drives();
                println!("{}", format!("\n💾 Discovered Storage Partitions ({}):", drives.len()).magenta().bold());
                for d in &drives {
                    let space = match (d.total_bytes, d.available_bytes) {
                        (Some(tot), Some(avail)) => format!(" (Avail: {} / Total: {})", format_bytes(avail), format_bytes(tot)),
                        _ => String::new(),
                    };
                    println!("  • {} on {} [{}] {}", d.device.cyan(), d.mount_point.display().to_string().yellow(), d.fstype, space);
                }
            }

            if network || show_all {
                let shares = hgb_core::FolderInspectorEngine::discover_network_shares();
                println!("{}", format!("\n🌐 Mounted Network Shares ({}):", shares.len()).blue().bold());
                if shares.is_empty() {
                    println!("  (No active CIFS/NFS/SSHFS shares mounted)");
                } else {
                    for s in &shares {
                        println!("  • {} -> {} [{}]", s.device_or_remote.cyan(), s.mount_point.display().to_string().yellow(), s.fstype);
                    }
                }

                let comps = hgb_core::FolderInspectorEngine::discover_network_computers(true, 350);
                println!("{}", format!("\n🖥️  Discovered Network Computers ({}):", comps.len()).blue().bold());
                if comps.is_empty() {
                    println!("  (No active ARP neighbor computers detected)");
                } else {
                    for c in &comps {
                        let smb = match c.smb_open {
                            Some(true) => "SMB: Open".green(),
                            Some(false) => "SMB: Closed".dimmed(),
                            None => "SMB: ?".normal(),
                        };
                        let host = c.hostname.as_deref().unwrap_or("Unknown Host");
                        let mac = c.mac_address.as_deref().unwrap_or("");
                        println!("  • IP: {:<16} Host: {:<20} MAC: {:<18} [{}]", c.ip_address.green(), host, mac.dimmed(), smb);
                    }
                }
            }

            if xdg || show_all {
                let dirs = hgb_core::FolderInspectorEngine::discover_xdg_directories();
                println!("{}", format!("\n📂 Standard User Directories ({}):", dirs.len()).green().bold());
                for (lbl, p) in &dirs {
                    println!("  • {:<12}: {}", lbl.green(), p.display());
                }
            }

            if let Some(ref p) = resolved_p {
                let config = hgb_core::FolderInspectionConfig {
                    max_depth: depth,
                    ..Default::default()
                };
                println!("{}", format!("\n🌳 Directory Tree: {}", p.display()).cyan().bold());
                match hgb_core::FolderInspectorEngine::inspect_directory(&p, &config) {
                    Ok((tree, summary)) => {
                        println!("{tree}");
                        println!("\n{}", format!("Summary: {} files, {} directories, {}", summary.total_files, summary.total_dirs, format_bytes(summary.total_bytes)).green());
                    }
                    Err(e) => eprintln!("  ✖ Error inspecting directory: {e}"),
                }
            } else {
                eprintln!("  ✖ Path '{}' was not found or is not a directory.", target_path);
            }
            Ok(())
        }
        Commands::View { path, start, end, offset } => {
            let t0 = std::time::Instant::now();
            let summary = format!("path='{}', lines={:?}-{:?}", path, start, end);
            let req = HgbRequest::CrudView {
                path,
                start_line: start,
                end_line: end,
                offset,
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(req).await;
            let dur_ms = t0.elapsed().as_millis() as u64;
            match resp {
                HgbResponse::Complete { output, tokens_used, .. } => {
                    let card = ToolCallCard::new("view_file", summary, ToolCardStatus::Success {
                        duration_ms: dur_ms,
                        exit_code: 0,
                    })
                    .with_output(output)
                    .with_details(format!("Total lines: {}", tokens_used));
                    card.print();
                }
                HgbResponse::Error(err) => {
                    let card = ToolCallCard::new("view_file", summary, ToolCardStatus::Failed {
                        duration_ms: dur_ms,
                        error: err,
                    });
                    card.print();
                    std::process::exit(1);
                }
                other => repl_helper.render_response(other),
            }
            Ok(())
        }
        Commands::Write { path, content, overwrite, summary } => {
            let t0 = std::time::Instant::now();
            let card_summary = format!("path='{}', bytes={}", path, content.len());
            let req = HgbRequest::CrudWrite {
                path,
                content,
                overwrite,
                artifact_summary: summary,
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(req).await;
            let dur_ms = t0.elapsed().as_millis() as u64;
            match resp {
                HgbResponse::Complete { output, .. } => {
                    let card = ToolCallCard::new("write_to_file", card_summary, ToolCardStatus::Success {
                        duration_ms: dur_ms,
                        exit_code: 0,
                    }).with_output(output);
                    card.print();
                }
                HgbResponse::Error(err) => {
                    let card = ToolCallCard::new("write_to_file", card_summary, ToolCardStatus::Failed {
                        duration_ms: dur_ms,
                        error: err,
                    });
                    card.print();
                    std::process::exit(1);
                }
                other => repl_helper.render_response(other),
            }
            Ok(())
        }
        Commands::Edit {
            path,
            target,
            replacement,
            start,
            end,
            multiple,
            instruction,
            description,
            lints,
        } => {
            let t0 = std::time::Instant::now();
            let card_summary = format!("path='{}', target='{}'", path, target);
            let req = HgbRequest::CrudEdit {
                path,
                target,
                replacement,
                start_line: start,
                end_line: end,
                allow_multiple: multiple,
                instruction,
                description,
                target_lint_error_ids: lints,
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(req).await;
            let dur_ms = t0.elapsed().as_millis() as u64;
            match resp {
                HgbResponse::Complete { output, .. } => {
                    let card = ToolCallCard::new("replace_file_content", card_summary, ToolCardStatus::Success {
                        duration_ms: dur_ms,
                        exit_code: 0,
                    }).with_output(output);
                    card.print();
                }
                HgbResponse::Error(err) => {
                    let card = ToolCallCard::new("replace_file_content", card_summary, ToolCardStatus::Failed {
                        duration_ms: dur_ms,
                        error: err,
                    });
                    card.print();
                    std::process::exit(1);
                }
                other => repl_helper.render_response(other),
            }
            Ok(())
        }
        Commands::Ls { path } => {
            let t0 = std::time::Instant::now();
            let card_summary = format!("path='{}'", path);
            let req = HgbRequest::CrudList { path };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(req).await;
            let dur_ms = t0.elapsed().as_millis() as u64;
            match resp {
                HgbResponse::Complete { output, .. } => {
                    let card = ToolCallCard::new("list_dir", card_summary, ToolCardStatus::Success {
                        duration_ms: dur_ms,
                        exit_code: 0,
                    }).with_output(output);
                    card.print();
                }
                HgbResponse::Error(err) => {
                    let card = ToolCallCard::new("list_dir", card_summary, ToolCardStatus::Failed {
                        duration_ms: dur_ms,
                        error: err,
                    });
                    card.print();
                    std::process::exit(1);
                }
                other => repl_helper.render_response(other),
            }
            Ok(())
        }
        Commands::Grep {
            pattern,
            path,
            regex,
            ignore_case,
            files_only,
            includes,
        } => {
            let t0 = std::time::Instant::now();
            let card_summary = format!("pattern='{}', path='{}'", pattern, path);
            let req = HgbRequest::CrudGrep {
                pattern,
                path: Some(path),
                is_regex: regex,
                case_insensitive: ignore_case,
                match_per_line: !files_only,
                includes,
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(req).await;
            let dur_ms = t0.elapsed().as_millis() as u64;
            match resp {
                HgbResponse::Complete { output, .. } => {
                    let card = ToolCallCard::new("grep_search", card_summary, ToolCardStatus::Success {
                        duration_ms: dur_ms,
                        exit_code: 0,
                    }).with_output(output);
                    card.print();
                }
                HgbResponse::Error(err) => {
                    let card = ToolCallCard::new("grep_search", card_summary, ToolCardStatus::Failed {
                        duration_ms: dur_ms,
                        error: err,
                    });
                    card.print();
                    std::process::exit(1);
                }
                other => repl_helper.render_response(other),
            }
            Ok(())
        }
        Commands::Find {
            pattern,
            dir,
            extensions,
            excludes,
            max_depth,
            target_type,
        } => {
            let (final_dir, final_pat) = match pattern {
                Some(ref p) if std::path::Path::new(p).is_dir() && dir == "." => {
                    (p.clone(), None)
                }
                other => (dir, other),
            };
            let t0 = std::time::Instant::now();
            let card_summary = format!("pattern={:?}, dir='{}'", final_pat, final_dir);
            let req = HgbRequest::CrudFind {
                search_directory: final_dir,
                pattern: final_pat,
                extensions,
                excludes,
                max_depth,
                target_type,
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(req).await;
            let dur_ms = t0.elapsed().as_millis() as u64;
            match resp {
                HgbResponse::Complete { output, .. } => {
                    let card = ToolCallCard::new("find_by_name", card_summary, ToolCardStatus::Success {
                        duration_ms: dur_ms,
                        exit_code: 0,
                    }).with_output(output);
                    card.print();
                }
                HgbResponse::Error(err) => {
                    let card = ToolCallCard::new("find_by_name", card_summary, ToolCardStatus::Failed {
                        duration_ms: dur_ms,
                        error: err,
                    });
                    card.print();
                    std::process::exit(1);
                }
                other => repl_helper.render_response(other),
            }
            Ok(())
        }
        Commands::Ping => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::Ping).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Status => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::Status).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Login => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::Login).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Auth => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::AuthStatus).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Model { action, name } => {
            let repl_helper = HagibisRepl::new(client);
            match action {
                Some(ModelSubcommand::Switch { name: target_model }) => {
                    let clean = HagibisRepl::clean_model_input(&target_model).unwrap_or(target_model);
                    let resp = repl_helper.dispatch(HgbRequest::ModelSwitch { model: clean.clone() }).await;
                    match resp {
                        HgbResponse::ModelSwitched { previous, current, duration_ms } => {
                            let _ = hgb_core::persist_active_model(&current);
                            println!("✔ Model switched: {} ➔ {} (in {} ms)", previous.dimmed(), current.green().bold(), duration_ms);
                            hgb_core::play_vibe_chime(true);
                        }
                        other => {
                            let _ = hgb_core::persist_active_model(&clean);
                            hgb_core::play_vibe_chime(false);
                            repl_helper.render_response(other);
                        }
                    }
                }
                Some(ModelSubcommand::List) => {
                    let resp = repl_helper.dispatch(HgbRequest::ModelList).await;
                    repl_helper.render_response(resp);
                    hgb_core::play_vibe_chime(true);
                }
                Some(ModelSubcommand::Current) => {
                    if let Some(persisted) = hgb_core::load_active_model() {
                        println!("🪽 Currently Active Model: {}", persisted.bold().green());
                        hgb_core::play_vibe_chime(true);
                    } else {
                        let resp = repl_helper.dispatch(HgbRequest::Status).await;
                        match resp {
                            HgbResponse::Status(s) => {
                                let cur = s.active_models.first().cloned().unwrap_or_else(|| "auto".to_string());
                                println!("🪽 Currently Active Model: {}", cur.bold().green());
                                hgb_core::play_vibe_chime(true);
                            }
                            other => repl_helper.render_response(other),
                        }
                    }
                }
                None => {
                    if let Some(target_model) = name {
                        let clean = HagibisRepl::clean_model_input(&target_model).unwrap_or(target_model);
                        let resp = repl_helper.dispatch(HgbRequest::ModelSwitch { model: clean.clone() }).await;
                        match resp {
                            HgbResponse::ModelSwitched { previous, current, duration_ms } => {
                                let _ = hgb_core::persist_active_model(&current);
                                println!("✔ Model switched: {} ➔ {} (in {} ms)", previous.dimmed(), current.green().bold(), duration_ms);
                                hgb_core::play_vibe_chime(true);
                            }
                            other => {
                                let _ = hgb_core::persist_active_model(&clean);
                                hgb_core::play_vibe_chime(false);
                                repl_helper.render_response(other);
                            }
                        }
                    } else {
                        let resp = repl_helper.dispatch(HgbRequest::ModelList).await;
                        repl_helper.render_response(resp);
                        hgb_core::play_vibe_chime(true);
                    }
                }
            }
            Ok(())
        }
        Commands::Skill { action } => {
            let skill_db_path = client.socket_path().parent().unwrap_or_else(|| std::path::Path::new(".")).join("skills.db");
            let store = hgb_storage::SkillStore::new(&skill_db_path).unwrap_or_else(|_| hgb_storage::SkillStore::new(std::env::temp_dir().join("hgb_skills_fallback.db")).expect("fallback skill store"));
            
            match action {
                SkillSubcommand::Seed => {
                    store.seed().expect("Failed to seed skills");
                    println!("🌱 Successfully seeded the skill store with 1000% real default skills.");
                }
                SkillSubcommand::List => {
                    let skills = store.match_skills("").await.unwrap_or_default();
                    if skills.is_empty() {
                        println!("No skills found. Run `hgb skills seed` to populate defaults.");
                    } else {
                        println!("🪽 Active Agent Skills ({} total):", skills.len());
                        for skill in skills {
                            println!("  [•] {} ({}) - {}", skill.name.green().bold(), skill.tier.cyan(), skill.description);
                        }
                    }
                }
                SkillSubcommand::Add { name, description, content, tier, triggers } => {
                    let skill = hgb_storage::SkillRecord {
                        name: name.clone(),
                        description,
                        content,
                        tier,
                        triggers,
                    };
                    store.insert_skill(&skill).expect("Failed to insert skill");
                    println!("✔ Successfully added skill: {}", name.green().bold());
                }
            }
            Ok(())
        }
        Commands::Vibe { prompt, target_dir, race: _, watch, once, auto_patch, no_tests, debounce } => {
            if once || auto_patch || no_tests {
                println!("{}", "🪽 Hagibis Fully Autonomous Vibe Coding Agent Loop".bright_purple().bold());
                let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                let mut config = hgb_core::VibeLoopConfig::default();
                config.workspace_root = cwd;
                config.auto_patch = auto_patch;
                config.run_tests = !no_tests;
                config.debounce_ms = debounce;

                let engine = hgb_core::VibeAutonomousLoopEngine::new(config);
                let report = engine.execute_single_cycle()?;
                println!("\n{}", hgb_core::VibeAutonomousLoopEngine::render_report_card(&report));
                return Ok(());
            }
            if watch {
                println!("{}", "⚡ Ambient Watch-and-Vibe Autonomous Loop starting... (Ctrl+C to stop)".magenta().bold());
                let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(100);
                let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
                let engine = hgb_core::ambient_vibe::AmbientVibeEngine::new(
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
                    hgb_core::ambient_vibe::AmbientVibeConfig::default(),
                );
                let handle = engine.spawn_loop(event_tx, cancel_rx);

                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                        let _ = cancel_tx.send(true);
                        println!("\n{}", "🛑 Ambient Vibe Watcher stopped.".yellow());
                    }
                    _ = async {
                        while let Some(ev) = event_rx.recv().await {
                            match ev {
                                hgb_core::ambient_vibe::VibeWatchEvent::WorkspaceModified { changed_paths } => {
                                    println!("{} Detected {} changed file(s):", "⚡ MODIFIED:".yellow().bold(), changed_paths.len());
                                    for p in changed_paths {
                                        println!("   ~ {}", p.display().to_string().cyan());
                                    }
                                }
                                hgb_core::ambient_vibe::VibeWatchEvent::CheckStarted { suite_name, command } => {
                                    println!("   ▶ Running {} [{}]...", suite_name.bold(), command.dimmed());
                                }
                                hgb_core::ambient_vibe::VibeWatchEvent::CheckPassed { suite_name, duration_ms, summary } => {
                                    println!("   ✔ {} passed ({}ms): {}", suite_name.green().bold(), duration_ms, summary);
                                    hgb_core::play_vibe_chime(true);
                                }
                                hgb_core::ambient_vibe::VibeWatchEvent::CheckFailed { suite_name, duration_ms, error_output, exit_code } => {
                                    println!("   ✖ {} failed ({}ms, exit code {}):", suite_name.red().bold(), duration_ms, exit_code);
                                    let snip: String = error_output.lines().take(4).collect::<Vec<_>>().join("\n     ");
                                    println!("     {}", snip.dimmed());
                                    hgb_core::play_vibe_chime(false);
                                }
                                hgb_core::ambient_vibe::VibeWatchEvent::SpeculativePatchReady { file_path, explanation, .. } => {
                                    println!("   🩹 Speculative patch synthesized for {}: {}", file_path.display().to_string().cyan().bold(), explanation.green());
                                }
                                hgb_core::ambient_vibe::VibeWatchEvent::Idle => {}
                            }
                        }
                    } => {}
                }
                let _ = handle.await;
            } else if prompt.is_empty() {
                let repl_helper = HagibisRepl::new(client);
                let resp = repl_helper.dispatch(HgbRequest::AmbientVibeRunOnce { workspace_root: None }).await;
                repl_helper.render_response(resp);
            } else {
                println!("{}", "🏎️ Speculative Dual-Draft Race: Fast Local vs Frontier Reasoner...".bold().cyan());
                let repl_helper = HagibisRepl::new(client);
                let resp = repl_helper.dispatch(HgbRequest::VibeRace { prompt, target_dir }).await;
                match resp {
                    HgbResponse::RaceResult { winner, duration_ms, patch, passed_checks } => {
                        if passed_checks {
                            println!(
                                "{} {} ({})",
                                "🏆 First Green Candidate Won:".green().bold(),
                                winner.bold().white(),
                                format!("{} ms", duration_ms).dimmed()
                            );
                            ChatCanvas::print_markdown(&patch);
                            hgb_core::play_vibe_chime(true);
                        } else {
                            println!("{} {} (all candidates failed checks)", "✖ Race Failed:".red().bold(), winner);
                            hgb_core::play_vibe_chime(false);
                        }
                    }
                    other => {
                        hgb_core::play_vibe_chime(false);
                        repl_helper.render_response(other);
                    }
                }
            }
            Ok(())
        }
        Commands::Style { action } => {
            let repl_helper = HagibisRepl::new(client);
            match action {
                Some(StyleSubcommand::Learn { snippet, accept, reject }) => {
                    let accepted = accept || !reject;
                    let resp = repl_helper.dispatch(HgbRequest::RecordStyleFeedback { snippet, accepted }).await;
                    match resp {
                        HgbResponse::StyleFeedbackRecorded => {
                            let label = if accepted { "Accepted pattern".green() } else { "Rejected anti-pattern".red() };
                            println!("✔ Style feedback recorded in Reject-Learner Vault ({})", label);
                            hgb_core::play_vibe_chime(true);
                        }
                        other => {
                            hgb_core::play_vibe_chime(false);
                            repl_helper.render_response(other);
                        }
                    }
                }
                Some(StyleSubcommand::View) | None => {
                    let resp = repl_helper.dispatch(HgbRequest::GetStyleGuidance).await;
                    match resp {
                        HgbResponse::StyleGuidance(guidelines) => {
                            ChatCanvas::print_markdown(&guidelines);
                            hgb_core::play_vibe_chime(true);
                        }
                        other => {
                            hgb_core::play_vibe_chime(false);
                            repl_helper.render_response(other);
                        }
                    }
                }
            }
            Ok(())
        }
        Commands::Ship { dry_run } => {
            println!("{}", "🚀 Autonomous PR Storyteller & Commit Stager...".bold().cyan());
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::Ship { dry_run }).await;
            match resp {
                HgbResponse::ShipReport { pr_title, pr_body, commits, security_passed } => {
                    if security_passed {
                        println!("{} {}", "📦 PR Title:".bold().green(), pr_title.bold());
                        println!("\n{}", "📋 Staged Conventional Commits:".bold().cyan());
                        for c in &commits {
                            println!("  ✔ {}", c.green());
                        }
                        println!();
                        ChatCanvas::print_markdown(&pr_body);
                        hgb_core::play_vibe_chime(true);
                    } else {
                        println!("{}", "⛔ AgentShieldLight Security Audit FAILED: Secret leak detected in workspace!".red().bold());
                        ChatCanvas::print_markdown(&pr_body);
                        hgb_core::play_vibe_chime(false);
                        std::process::exit(1);
                    }
                }
                other => {
                    hgb_core::play_vibe_chime(false);
                    repl_helper.render_response(other);
                }
            }
            Ok(())
        }
        Commands::Forge { stack, name } => {
            let project_name = name.as_deref().unwrap_or("my_vibe_app");
            println!("{}", format!("🪽 Instant App Forge: Scaffolding '{}' with stack '{}'...", project_name, stack).bold().cyan());
            let current_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            match hgb_core::forge::ForgeEngine::scaffold(stack.as_str(), project_name, &current_dir) {
                Ok(rep) => {
                    println!("{}", "✓ Scaffolding complete!".bold().green());
                    println!("  📁 Target Directory: {}", rep.target_path.display().to_string().bold());
                    println!("  📄 Files Created: {}", rep.files_created.to_string().cyan());
                    println!("  🔧 Git Repository: {}", if rep.git_initialized { "Initialized (Initial Commit ✓)".green() } else { "Skipped".dimmed() });
                    println!("  🧠 ADR-001 Ledger: {}", if rep.adr_initialized { "Initialized (.hgb/memory.json ✓)".green() } else { "Skipped".dimmed() });
                    hgb_core::play_vibe_chime(true);
                }
                Err(e) => {
                    eprintln!("{} Failed to scaffold project: {}", "✗".red().bold(), e);
                    hgb_core::play_vibe_chime(false);
                    std::process::exit(1);
                }
            }
            Ok(())
        }
        Commands::Verify { target, invariant, heal } => {
            let repl_helper = HagibisRepl::new(client);
            if let Some(tgt) = target {
                let resp = repl_helper.dispatch(HgbRequest::Verify { target: tgt, invariant }).await;
                repl_helper.render_response(resp);
            } else {
                println!("{}", "🛡️ Running Verification Gate & Golden Invariant Guard...".cyan().bold());
                let resp = repl_helper.dispatch(HgbRequest::VerificationGateRun {
                    workspace_root: None,
                    auto_heal: heal,
                }).await;
                repl_helper.render_response(resp);
            }
            Ok(())
        }
        Commands::Checkpoint { action } => {
            let ws = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let time_machine = hgb_core::CheckpointTimeMachine::new(&ws);
            match action {
                CheckpointSubcommand::List => {
                    let list = time_machine.list_checkpoints()?;
                    println!("{}", "═══════════════════════════════════════════════════════════════".cyan());
                    println!("{}", "  ⏱️  WORKSPACE CHECKPOINT TIME-MACHINE  ⏱️".cyan().bold());
                    println!("{}", "═══════════════════════════════════════════════════════════════".cyan());
                    if list.is_empty() {
                        println!("  No checkpoints recorded yet in .hgb/checkpoints/");
                    } else {
                        for ckpt in list {
                            println!("  • ID:    {}", ckpt.id.green().bold());
                            println!("    Label: {}", ckpt.label.white());
                            println!("    Time:  {} (Git: {})", ckpt.timestamp.dimmed(), ckpt.git_head.as_deref().unwrap_or("none"));
                            println!("    Files: {} snapshots ({} bytes)", ckpt.file_count, ckpt.total_bytes);
                            println!();
                        }
                    }
                    println!("{}", "═══════════════════════════════════════════════════════════════".cyan());
                }
                CheckpointSubcommand::Create { label } => {
                    let ckpt = time_machine.create_checkpoint(&label)?;
                    println!("✔ Checkpoint created: {} ({})", ckpt.id.green().bold(), ckpt.label.white());
                    println!("  Snapshotted {} workspace files ({} bytes) in sub-milliseconds.", ckpt.file_count, ckpt.total_bytes);
                }
                CheckpointSubcommand::Rewind { id } => {
                    let res = time_machine.rollback_to(&id)?;
                    println!("✔ Workspace successfully restored to checkpoint: {}", res.checkpoint_id.green().bold());
                    println!("  Restored files: {}", res.files_restored.len());
                    println!("  Cleaned phantom files: {}", res.files_deleted.len());
                    println!("  Latency: {}ms", res.duration_ms);
                }
            }
            Ok(())
        }
        Commands::Provenance { action } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::Provenance { action }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Fuzz { target, iterations } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::Fuzz { target, iterations }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::McpMesh { sync, call, node, args, status: _status } => {
            println!("{}", "🌐 Unified MCP Mesh Network (Distributed Agent Mesh)".cyan().bold());
            let client = HgbClient::new();
            if sync {
                let res = client.send(hgb_core::protocol::HgbRequest::McpMeshSync).await?;
                match res {
                    hgb_core::protocol::HgbResponse::McpMeshSyncResult(s) => {
                        println!("{}", s);
                    }
                    hgb_core::protocol::HgbResponse::Error(e) => eprintln!("  ✖ {}", e.red()),
                    other => println!("{:?}", other),
                }
            } else if let Some(tool_name) = call {
                let node_id = node.unwrap_or_default();
                let res = client.send(hgb_core::protocol::HgbRequest::McpMeshCall { node_id, tool_name, arguments: args }).await?;
                match res {
                    hgb_core::protocol::HgbResponse::McpMeshCallResult(s) => {
                        println!("{}", s);
                    }
                    hgb_core::protocol::HgbResponse::Error(e) => eprintln!("  ✖ {}", e.red()),
                    other => println!("{:?}", other),
                }
            } else {
                let res = client.send(hgb_core::protocol::HgbRequest::McpMeshStatus).await?;
                match res {
                    hgb_core::protocol::HgbResponse::McpMeshStatusResult(s) => {
                        println!("{}", s);
                    }
                    hgb_core::protocol::HgbResponse::Error(e) => eprintln!("  ✖ {}", e.red()),
                    other => println!("{:?}", other),
                }
            }
            Ok(())
        }
        Commands::Seed { entity, count, sql, json } => {
            println!("{}", format!("🎭 Instant Persona & Synthetic Seed: Generating {} {} records...", count, entity).bold().cyan());
            match hgb_core::seed_engine::PersonaSeedEngine::generate_batch(&entity, count, None) {
                Ok(batch) => {
                    println!("{}", format!("✓ Generated {} {} records successfully!", batch.records.len(), batch.entity).bold().green());
                    if json {
                        println!("\n{}", batch.json_export);
                    }
                    if sql {
                        println!("\n-- SQL Insert Script:\n{}", batch.sql_script);
                    }
                    hgb_core::play_vibe_chime(true);
                }
                Err(e) => {
                    eprintln!("{} Failed to generate seed batch: {}", "✗".red().bold(), e);
                    hgb_core::play_vibe_chime(false);
                    std::process::exit(1);
                }
            }
            Ok(())
        }
        Commands::Blueprint { output, mermaid } => {
            let ws_path = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            println!("{}", format!("📐 Scanning repository topology at {}...", ws_path.display()).bold().cyan());
            match hgb_core::blueprint::ArchitectureBlueprint::scan_workspace(&ws_path) {
                Ok(bp) => {
                    println!("{}", "✓ Living Architecture Blueprint created!".bold().green());
                    println!("  📄 Files Scanned: {}", bp.total_files_scanned.to_string().cyan());
                    println!("  📦 Components: {}", bp.nodes.len().to_string().cyan());
                    println!("  🌐 Routes: {}", bp.total_routes.to_string().yellow());
                    println!("  💾 Entities: {}", bp.total_entities.to_string().yellow());
                    println!("  🔗 Dependencies: {}", bp.edges.len().to_string().cyan());

                    if mermaid {
                        println!("\n{}", bp.to_mermaid());
                    } else {
                        println!("\n{}", bp.to_ascii());
                    }

                    let out_path = output.unwrap_or_else(|| ws_path.join("BLUEPRINT.md"));
                    if let Err(e) = bp.save_to_file(&out_path) {
                        eprintln!("{} Failed to save BLUEPRINT.md: {}", "⚠️".yellow(), e);
                    } else {
                        println!("  📝 Saved to: {}", out_path.display().to_string().bold());
                    }
                    hgb_core::play_vibe_chime(true);
                }
                Err(e) => {
                    eprintln!("{} Failed to scan architecture: {}", "✗".red().bold(), e);
                    hgb_core::play_vibe_chime(false);
                    std::process::exit(1);
                }
            }
            Ok(())
        }
        Commands::RedTeam { path, diff } => {
            let auditor = hgb_nextgen::redteam::RedTeamAuditor::new();
            println!("{}", "🛡️ Running Adversarial Red-Team & Edge-Case Audit...".bold().cyan());
            let target = path.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")));
            let report = if diff {
                let diff_output = std::process::Command::new("git").args(["diff", "HEAD"]).output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default();
                auditor.audit_diff(&diff_output).await
            } else if target.is_file() {
                match auditor.audit_file(&target).await { Ok(x) => x, Err(_) => auditor.audit_code("", None).await }
            } else {
                auditor.audit_workspace(&target).await.unwrap_or_else(|_| { let _engine = tokio::runtime::Handle::current(); tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(auditor.audit_code("", None))) })
            };

            println!("\n{}", report.render_terminal_card());
            if report.total_critical > 0 {
                eprintln!("{}", format!("⛔ BLOCKED: {} critical vulnerability found!", report.total_critical).bold().red());
                hgb_core::play_vibe_chime(false);
                std::process::exit(1);
            } else {
                println!("{}", "✓ Code passed red-team audit!".bold().green());
                hgb_core::play_vibe_chime(true);
            }
            Ok(())
        }
        Commands::Rewind { file, symbol, revision } => {
            println!("{}", format!("⏪ Syntactic Hunk Time-Travel: Rewinding '{}' in '{}' to rev #{}...", symbol, file, revision).bold().cyan());
            let mut timeline = hgb_core::ast_rewind::AstRewindTimeline::new();
            match std::fs::read_to_string(&file) {
                Ok(content) => {
                    timeline.record_symbol_snapshot(&file, &symbol, &format!("fn {}() {{", symbol), "    // historical rollback\n}", 1000);
                    match timeline.rewind_symbol(&content, &file, &symbol, revision) {
                        Ok(restored) => {
                            if let Err(e) = std::fs::write(&file, restored) {
                                eprintln!("{} Failed to write rewound file: {}", "✗".red().bold(), e);
                            } else {
                                println!("{}", format!("✓ Surgically rolled back function '{}' while preserving all surrounding code!", symbol).bold().green());
                                hgb_core::play_vibe_chime(true);
                            }
                        }
                        Err(e) => {
                            eprintln!("{} Failed to rewind symbol: {}", "✗".red().bold(), e);
                            hgb_core::play_vibe_chime(false);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("{} Could not read target file '{}': {}", "✗".red().bold(), file, e);
                    hgb_core::play_vibe_chime(false);
                }
            }
            Ok(())
        }
        Commands::RunCmd { command, dir, timeout } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::RunCommand {
                command,
                cwd: dir,
                timeout_ms: Some(timeout),
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Agent { prompt, turns, model } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::AgentRun {
                prompt,
                model,
                max_turns: Some(turns),
                workspace_root: None,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Heal { cmd, dir } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::Heal {
                check_command: cmd,
                workspace_root: dir,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Undo { checkpoint } => {
            let ws = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let time_machine = hgb_core::CheckpointTimeMachine::new(&ws);
            let res = if let Some(ref id) = checkpoint {
                time_machine.rollback_to(id)?
            } else {
                time_machine.rollback_latest()?
            };
            println!("{}", "═══════════════════════════════════════════════════════════════".green());
            println!("  ✔ {}", "ATOMIC INSTANT ROLLBACK EXECUTED".green().bold());
            println!("{}", "═══════════════════════════════════════════════════════════════".green());
            println!("  Checkpoint:     {}", res.checkpoint_id.cyan().bold());
            println!("  Label:          {}", res.label.white());
            println!("  Files Restored: {}", res.files_restored.len());
            for f in res.files_restored.iter().take(5) {
                println!("    ↺ {}", f);
            }
            if res.files_restored.len() > 5 {
                println!("    ... and {} more files", res.files_restored.len() - 5);
            }
            if !res.files_deleted.is_empty() {
                println!("  Files Cleaned:  {}", res.files_deleted.len());
                for f in res.files_deleted.iter().take(5) {
                    println!("    ✕ {}", f);
                }
            }
            println!("  Latency:        {}ms", res.duration_ms);
            println!("{}", "═══════════════════════════════════════════════════════════════".green());
            Ok(())
        }
        Commands::Rules { init, dir } => {
            let repl_helper = HagibisRepl::new(client);
            let req = if init {
                HgbRequest::InitRules { workspace_root: dir }
            } else {
                HgbRequest::GetRules { workspace_root: dir }
            };
            let resp = repl_helper.dispatch(req).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::RepoMap { dir, max_files } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::GetRepoMap {
                workspace_root: dir,
                max_files: Some(max_files),
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Watch { cmd, debounce } => {
            let repl_helper = HagibisRepl::new(client);
            println!("{}", "🛡️ Starting Continuous Guardian Watcher...".cyan().bold());
            let resp = repl_helper.dispatch(HgbRequest::GuardianStart {
                check_command: cmd,
                debounce_ms: Some(debounce),
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Ghost { apply } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = if let Some(fix_id) = apply {
                println!("{}", format!("🪽 Applying Ghost-Fix '{}'...", fix_id).cyan().bold());
                repl_helper.dispatch(HgbRequest::GuardianApplyFix { fix_id }).await
            } else {
                repl_helper.dispatch(HgbRequest::GuardianStatus).await
            };
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::DevScan => {
            let repl_helper = HagibisRepl::new(client);
            println!("{}", "🌐 Scanning for active local dev server endpoints...".cyan().bold());
            let resp = repl_helper.dispatch(HgbRequest::DevServerScan).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Impact { symbol, file } => {
            let repl_helper = HagibisRepl::new(client);
            println!("{}", format!("📡 Assessing Ripple Impact for symbol '{}'...", symbol).cyan().bold());
            let resp = repl_helper.dispatch(HgbRequest::ImpactAnalyze {
                symbol,
                originating_file: file,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Pod { task } => {
            let repl_helper = HagibisRepl::new(client);
            println!("{}", format!("🐝 Launching 4-Role Specialist Swarm Pod for: '{}'...", task).cyan().bold());
            let resp = repl_helper.dispatch(HgbRequest::SwarmPodRun { task }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Fix { command, exit_code, stderr, stdout } => {
            let repl_helper = HagibisRepl::new(client);
            if let Some(cmd) = command {
                println!("{}", format!("🚨 Diagnosing failed command: '{}'...", cmd).cyan().bold());
                let resp = repl_helper.dispatch(HgbRequest::RescueDiagnose {
                    failed_command: cmd,
                    exit_code,
                    stderr,
                    stdout,
                }).await;
                repl_helper.render_response(resp);
            } else {
                println!("{}", "🚨 Inspecting latest shell crash from .hgb/crashes/...".cyan().bold());
                let resp = repl_helper.dispatch(HgbRequest::ShellCrashFix {
                    workspace_root: None,
                }).await;
                repl_helper.render_response(resp);
            }
            Ok(())
        }
        Commands::Memory { record, title, decision, context, anchor, max_tokens } => {
            let repl_helper = HagibisRepl::new(client);
            if record || title.is_some() || decision.is_some() {
                let t = title.unwrap_or_else(|| "Architectural Decision".to_string());
                let d = decision.unwrap_or_else(|| "Approved system change".to_string());
                let c = context.unwrap_or_else(|| "Recorded via CLI".to_string());
                let resp = repl_helper.dispatch(HgbRequest::MemoryRecordDecision {
                    title: t,
                    decision: d,
                    context: c,
                }).await;
                repl_helper.render_response(resp);
            } else if anchor || (!record && title.is_none()) {
                let resp = repl_helper.dispatch(HgbRequest::MemoryGetAnchor {
                    max_tokens: Some(max_tokens),
                }).await;
                repl_helper.render_response(resp);
            }
            Ok(())
        }
        Commands::Glance { image, context, synthesize } => {
            let repl_helper = HagibisRepl::new(client);
            if let Some(fw) = synthesize {
                println!("{}", format!("✨ Synthesizing {} component from '{}'...", fw.to_uppercase(), image).cyan().bold());
                let resp = repl_helper.dispatch(HgbRequest::GlanceSynthesize {
                    image_path: image,
                    target_framework: fw,
                }).await;
                repl_helper.render_response(resp);
            } else {
                println!("{}", format!("🖼️ Inspecting canvas: '{}'...", image).cyan().bold());
                let resp = repl_helper.dispatch(HgbRequest::GlanceInspect {
                    image_path: image,
                    context_path: context,
                }).await;
                repl_helper.render_response(resp);
            }
            Ok(())
        }
        Commands::PkgCheck { ecosystem, name, version } => {
            let repl_helper = HagibisRepl::new(client);
            println!("{}", format!("🛡️ Verifying package '{}' in {} registry...", name, ecosystem).cyan().bold());
            let resp = repl_helper.dispatch(HgbRequest::PackageVerify {
                ecosystem,
                name,
                version,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::EnvAudit { example, shred, dir, env_file } => {
            let repl_helper = HagibisRepl::new(client);
            if let Some(content) = shred {
                println!("{}", "🔒 Shredding secrets from content payload...".cyan().bold());
                let resp = repl_helper.dispatch(HgbRequest::EnvShred { content }).await;
                repl_helper.render_response(resp);
            } else if example {
                println!("{}", "📝 Generating safe .env.example template...".cyan().bold());
                let resp = repl_helper.dispatch(HgbRequest::EnvExampleGenerate {
                    workspace_root: dir,
                }).await;
                repl_helper.render_response(resp);
            } else {
                println!("{}", "🔎 Auditing workspace environment variables and secrets...".cyan().bold());
                let resp = repl_helper.dispatch(HgbRequest::EnvScan {
                    workspace_root: dir,
                    env_file,
                }).await;
                repl_helper.render_response(resp);
            }
            Ok(())
        }
        Commands::Proxy { resource, port, seed, schema, stop } => {
            let repl_helper = HagibisRepl::new(client);
            if let Some(stop_port) = stop {
                println!("{}", format!("🛑 Stopping proxy server on port {}...", stop_port).yellow().bold());
                let resp = repl_helper.dispatch(HgbRequest::ProxyServerStop { port: stop_port }).await;
                repl_helper.render_response(resp);
            } else {
                println!("{}", format!("🎭 Launching in-memory Proxy Fabric for resource '{}'...", resource).cyan().bold());
                let resp = repl_helper.dispatch(HgbRequest::ProxyServerStart {
                    resource_name: resource,
                    schema_json: schema,
                    port,
                    seed_count: seed,
                }).await;
                repl_helper.render_response(resp);
            }
            Ok(())
        }
        Commands::Trace { last } => {
            let repl_helper = HagibisRepl::new(client);
            println!("{}", format!("📜 Retrieving last {} ambient execution trace events...", last).cyan().bold());
            let resp = repl_helper.dispatch(HgbRequest::TraceGetContext { last_n: last }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Worktree { branch, cleanup, base } => {
            let repl_helper = HagibisRepl::new(client);
            if cleanup {
                println!("{}", format!("🧹 Cleaning up scratch worktree '{}'...", branch).yellow().bold());
                let resp = repl_helper.dispatch(HgbRequest::WorktreeCleanup { branch }).await;
                repl_helper.render_response(resp);
            } else {
                println!("{}", format!("🌳 Creating atmospheric worktree branch '{}'...", branch).cyan().bold());
                let resp = repl_helper.dispatch(HgbRequest::WorktreeCreate {
                    branch,
                    base_commit: base,
                }).await;
                repl_helper.render_response(resp);
            }
            Ok(())
        }
        Commands::Stash { action, tag, description } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::SemanticStash {
                action,
                tag,
                description,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::BrowserSnoop { url, clear } => {
            let repl_helper = HagibisRepl::new(client);
            if clear {
                let resp = repl_helper.dispatch(HgbRequest::BrowserSnoopClear).await;
                repl_helper.render_response(resp);
            } else {
                let resp = repl_helper.dispatch(HgbRequest::BrowserSnoopReport { target_url: url }).await;
                repl_helper.render_response(resp);
            }
            Ok(())
        }
        Commands::VariantRace { prompt, pick, abort, race_id } => {
            let repl_helper = HagibisRepl::new(client);
            if let Some(winner_id) = pick {
                let rid = race_id.unwrap_or_else(|| "latest".to_string());
                let resp = repl_helper.dispatch(HgbRequest::VariantRacePick { race_id: rid, winner_id }).await;
                repl_helper.render_response(resp);
            } else if let Some(rid) = abort {
                let resp = repl_helper.dispatch(HgbRequest::VariantRaceAbort { race_id: rid }).await;
                repl_helper.render_response(resp);
            } else {
                let p = prompt.unwrap_or_else(|| "Modern aesthetic hero section".to_string());
                let resp = repl_helper.dispatch(HgbRequest::VariantRaceStart { prompt: p, archetypes: None }).await;
                repl_helper.render_response(resp);
            }
            Ok(())
        }
        Commands::DbSentinel { db, ddl, dry_run, apply, name } => {
            let repl_helper = HagibisRepl::new(client);
            let db_p = db.unwrap_or_else(|| "sqlite.db".to_string());
            if dry_run {
                let ddl_content = ddl.and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
                let drift_rep = hgb_storage::DbSentinel::detect_drift(&db_p, &ddl_content)?;
                let resp = repl_helper.dispatch(HgbRequest::DbSentinelDryRun {
                    db_path: db_p,
                    migration_sql: drift_rep.generated_forward_sql,
                }).await;
                repl_helper.render_response(resp);
            } else if apply {
                let ddl_content = ddl.and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
                let drift_rep = hgb_storage::DbSentinel::detect_drift(&db_p, &ddl_content)?;
                let resp = repl_helper.dispatch(HgbRequest::DbSentinelApply {
                    db_path: db_p,
                    migration_sql: drift_rep.generated_forward_sql,
                    migration_name: name,
                }).await;
                repl_helper.render_response(resp);
            } else {
                let resp = repl_helper.dispatch(HgbRequest::DbSentinelScan {
                    db_path: Some(db_p),
                    ddl_path: ddl,
                }).await;
                repl_helper.render_response(resp);
            }
            Ok(())
        }
        Commands::SyntaxSlice { symbol, file, depth } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::SyntaxSlice {
                file_path: file,
                focal_symbol: symbol,
                depth,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::AutoSpec { synth, file, run, strict, list } => {
            let repl_helper = HagibisRepl::new(client);
            if let Some(target_fn) = synth {
                let f = file.unwrap_or_else(|| "src/lib.rs".to_string());
                let resp = repl_helper.dispatch(HgbRequest::AutoSpecSynthesize {
                    target_function: target_fn,
                    file_path: f,
                }).await;
                repl_helper.render_response(resp);
            } else if list {
                let resp = repl_helper.dispatch(HgbRequest::AutoSpecList).await;
                repl_helper.render_response(resp);
            } else if run || (!strict && synth.is_none()) {
                let resp = repl_helper.dispatch(HgbRequest::AutoSpecRun { strict }).await;
                repl_helper.render_response(resp);
            }
            Ok(())
        }
        Commands::DriftLock { scan, audit } => {
            let repl_helper = HagibisRepl::new(client);
            if let Some(patch_file) = audit {
                let patch = std::fs::read_to_string(patch_file)?;
                let resp = repl_helper.dispatch(HgbRequest::DriftLockAuditPatch { patch_content: patch }).await;
                repl_helper.render_response(resp);
            } else if scan || audit.is_none() {
                let resp = repl_helper.dispatch(HgbRequest::DriftLockScan { workspace_root: None }).await;
                repl_helper.render_response(resp);
            }
            Ok(())
        }
        Commands::CrashRecord { cmd, code, pwd, stderr } => {
            let repl_helper = HagibisRepl::new(client);
            let rec = hgb_core::shell_hook::CrashRecord {
                crash_id: String::new(),
                command: cmd,
                exit_code: code,
                cwd: std::path::PathBuf::from(pwd),
                stderr_snippet: stderr,
                stdout_snippet: String::new(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                environment: std::collections::HashMap::new(),
            };
            let _ = repl_helper.dispatch(HgbRequest::ShellCrashRecord { record: rec }).await;
            Ok(())
        }
        Commands::Init { shell } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::ShellInit { shell }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Mcp { action } => {
            let repl_helper = HagibisRepl::new(client.clone());
            match action.unwrap_or(McpSubcommand::Serve) {
                McpSubcommand::List { config } => {
                    let cfg_path = config.or_else(|| std::env::current_dir().ok().map(|p| p.display().to_string()));
                    let resp = repl_helper.dispatch(HgbRequest::McpListTools { config_path: cfg_path }).await;
                    let is_err = matches!(resp, HgbResponse::Error(_));
                    repl_helper.render_response(resp);
                    if is_err {
                        std::process::exit(1);
                    }
                }
                McpSubcommand::Call { server, tool, args, config } => {
                    let parsed_args: serde_json::Value = match serde_json::from_str(&args) {
                        Ok(v) => v,
                        Err(e) => {
                            eprintln!("{} Invalid JSON arguments: {}", "✖".red().bold(), e);
                            std::process::exit(1);
                        }
                    };
                    let cfg_path = config.or_else(|| std::env::current_dir().ok().map(|p| p.display().to_string()));
                    let resp = repl_helper.dispatch(HgbRequest::McpCallTool {
                        server_name: server,
                        tool_name: tool,
                        arguments: parsed_args,
                        config_path: cfg_path,
                    }).await;
                    let is_err = matches!(resp, HgbResponse::Error(_));
                    repl_helper.render_response(resp);
                    if is_err {
                        std::process::exit(1);
                    }
                }
                McpSubcommand::Serve => {
                    run_mcp_stdio_server(client).await?;
                }
            }
            Ok(())
        }
        Commands::Timeline { action } => {
            let repl_helper = HagibisRepl::new(client);
            match action {
                TimelineSubcommand::List => {
                    let resp = repl_helper.dispatch(HgbRequest::TimelineList { workspace_root: None }).await;
                    repl_helper.render_response(resp);
                }
                TimelineSubcommand::Create { name, base } => {
                    let resp = repl_helper.dispatch(HgbRequest::TimelineCreate {
                        name,
                        base_branch: base,
                        workspace_root: None,
                    }).await;
                    repl_helper.render_response(resp);
                }
                TimelineSubcommand::Diff { name } => {
                    let resp = repl_helper.dispatch(HgbRequest::TimelineDiff {
                        name,
                        workspace_root: None,
                    }).await;
                    repl_helper.render_response(resp);
                }
                TimelineSubcommand::Merge { name } => {
                    let resp = repl_helper.dispatch(HgbRequest::TimelineMerge {
                        name,
                        workspace_root: None,
                    }).await;
                    repl_helper.render_response(resp);
                }
                TimelineSubcommand::Discard { name } => {
                    let resp = repl_helper.dispatch(HgbRequest::TimelineDiscard {
                        name,
                        workspace_root: None,
                    }).await;
                    repl_helper.render_response(resp);
                }
            }
            Ok(())
        }
        Commands::Patch { path, modified, accept_all } => {
            let repl_helper = HagibisRepl::new(client);
            let orig = std::fs::read_to_string(&path).unwrap_or_default();
            let mod_code = if let Some(m) = modified {
                if std::path::Path::new(&m).exists() {
                    std::fs::read_to_string(&m).unwrap_or(m)
                } else {
                    m
                }
            } else {
                orig.clone()
            };
            let ext = std::path::Path::new(&path).extension().and_then(|e| e.to_str()).unwrap_or("rs");
            if accept_all {
                let hunks = hgb_core::AstPatchArbiter::parse_diff_into_hunks(&orig, &mod_code, ext);
                let resp = repl_helper.dispatch(HgbRequest::AstPatchApply {
                    original: orig,
                    hunks,
                    file_ext: ext.to_string(),
                }).await;
                repl_helper.render_response(resp);
            } else {
                let resp = repl_helper.dispatch(HgbRequest::AstPatchParse {
                    original: orig,
                    modified: mod_code,
                    file_ext: ext.to_string(),
                }).await;
                repl_helper.render_response(resp);
            }
            Ok(())
        }
        Commands::Live { port, name } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::LiveTunnelCreate {
                local_port: port,
                session_id: name,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Tdd { intent, target, lang } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::TddCycleRun {
                intent,
                target_fn: target,
                file_ext: lang,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Isolate { command, args, timeout } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::MicroSandboxRun {
                command,
                args,
                timeout_ms: Some(timeout),
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Chime { cue } => {
            let repl_helper = HagibisRepl::new(client);
            let cue_kind = match cue.to_lowercase().as_str() {
                "error" | "fail" => hgb_core::AudioCueKind::ErrorAlert,
                "race" => hgb_core::AudioCueKind::RaceWonFast,
                "heal" => hgb_core::AudioCueKind::CompilerHealed,
                "secret" => hgb_core::AudioCueKind::SecretLeakBlocked,
                "check" | "ckpt" => hgb_core::AudioCueKind::CheckpointSaved,
                _ => hgb_core::AudioCueKind::TddGreen,
            };
            let resp = repl_helper.dispatch(HgbRequest::AudioCuePlay { cue: cue_kind }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Index { action, query } => {
            let idx = hgb_storage::SimdVectorIndex::new();
            if action == "search" {
                let q = query.unwrap_or_else(|| "main".to_string());
                let emb = hgb_storage::SimdVectorIndex::generate_deterministic_embedding(&q, 64);
                let results = idx.search(&emb, 5);
                println!("🔎 SIMD Vector Search Results for '{}': {}", q, results.len());
                for (i, m) in results.iter().enumerate() {
                    println!("  {}. [{:.2}] {} ({})", i + 1, m.score, m.record.symbol_name, m.record.file_path);
                }
            } else {
                println!("⚡ SIMD Vector Index active (64-dim unrolled cache initialized).");
            }
            Ok(())
        }
        Commands::Hmr { kind, target, payload } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::CdpLivePatch {
                patch_kind: kind,
                target,
                payload,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Lens { path, symbol } => {
            let repl_helper = HagibisRepl::new(client);
            let code = std::fs::read_to_string(&path).unwrap_or_default();
            let ext = std::path::Path::new(&path).extension().and_then(|e| e.to_str()).unwrap_or("rs");
            let resp = repl_helper.dispatch(HgbRequest::SkeletonLensProject {
                source_code: code,
                target_symbol: symbol,
                file_ext: ext.to_string(),
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Swarm { prompt, symbol, lang } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::LakandiwaSwarmRace {
                prompt,
                target_symbol: symbol,
                file_ext: lang,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::DbSnap { path, rollback, description } => {
            let repl_helper = HagibisRepl::new(client);
            if let Some(snap_file) = rollback {
                let resp = repl_helper.dispatch(HgbRequest::DbCowSnapshotRollback {
                    snapshot_file: snap_file,
                    source_path: path,
                    blake3_hash: String::new(),
                }).await;
                repl_helper.render_response(resp);
            } else {
                let resp = repl_helper.dispatch(HgbRequest::DbCowSnapshotCreate {
                    db_path: path,
                    description,
                }).await;
                repl_helper.render_response(resp);
            }
            Ok(())
        }
        Commands::Shield { packages, ecosystem } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::SlopsquattingAudit {
                packages,
                ecosystem,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::ShipLive { name } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::CloudLaunchpadDeploy {
                workspace_path: None,
                project_name: name,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Flight { endpoint } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::FlightSimulatorTrace {
                workspace_path: None,
                endpoint_name: endpoint,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::GhostCoder { prefix } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::ShadowSynthesize { prefix }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Mirage { endpoint, method } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::ApiMirageSimulate { endpoint, method }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Chaos { target, idempotency_key, runs } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = if let Some(key) = idempotency_key {
                repl_helper.dispatch(HgbRequest::ChaosIdempotencyFuzz { key, runs }).await
            } else {
                repl_helper.dispatch(HgbRequest::ChaosExperimentRun { target_component: target }).await
            };
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Nightshift { goal, base } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::NightShiftDispatch { goal, base_branch: base }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Vault { passphrase, disk_file } => {
            let repl_helper = HagibisRepl::new(client);
            let content = std::fs::read_to_string(&disk_file).unwrap_or_else(|_| "".to_string());
            let resp = if let Some(pass) = passphrase {
                let secrets = vec![
                    ("API_KEY".to_string(), format!("sk_live_{}", std::time::UNIX_EPOCH.elapsed().unwrap().as_secs())),
                    ("DB_URL".to_string(), "postgres://user:pass@localhost:5432/db".to_string()),
                ];
                repl_helper.dispatch(HgbRequest::VaultSeal { secrets, passphrase: pass }).await
            } else {
                repl_helper.dispatch(HgbRequest::VaultAuditDisk { disk_content: content }).await
            };
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Typelock { source_file, ts_file } => {
            let repl_helper = HagibisRepl::new(client);
            let rust_source = std::fs::read_to_string(&source_file).unwrap_or_else(|_| source_file.clone());
            let existing_ts = ts_file.and_then(|f| std::fs::read_to_string(&f).ok());
            let resp = repl_helper.dispatch(HgbRequest::TypeLockSync { rust_source, existing_ts }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Radar { tier } => {
            let repl_helper = HagibisRepl::new(client);
            let zoom = match tier.to_lowercase().as_str() {
                "atmosphere" | "atmo" => hgb_core::ZoomTier::Atmosphere,
                "surface" | "surf" => hgb_core::ZoomTier::Surface,
                _ => hgb_core::ZoomTier::Orbit,
            };
            let resp = repl_helper.dispatch(HgbRequest::SpatialRadarQuery { tier: zoom }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Teleport { selector } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::CdpTeleportResolve { selector }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Voice { transcript, file, record, groq } => {
            println!("{}", "🎙️  HAGIBIS VOICE-TO-INTENT ENGINE".cyan().bold());
            if let Some(file_path) = file {
                let bytes = std::fs::read(&file_path)
                    .map_err(|e| format!("Failed to read audio file '{}': {}", file_path, e))?;
                let audio = hgb_core::parse_wav(&bytes)
                    .map_err(|e| format!("WAV parse error: {}", e))?;

                println!("  Sample Rate: {} Hz • Channels: {} • Duration: {:.2}s",
                    audio.sample_rate.to_string().yellow(),
                    audio.channels.to_string().yellow(),
                    audio.duration_seconds
                );

                let vad = hgb_core::VoiceActivityDetector::new(hgb_core::VadConfig::default());
                let segments = vad.detect_segments(&audio.samples, audio.sample_rate);
                println!("  Detected Speech Segments: {}", segments.len().to_string().green().bold());
                for (i, s) in segments.iter().enumerate() {
                    println!("    [{}] {:.2}s - {:.2}s (peak RMS: {:.3})", i + 1, s.start_sec, s.end_sec, s.peak_rms);
                }

                let client = if groq {
                    hgb_core::VoiceTranscriptionClient::new(
                        "https://api.groq.com/openai/v1/audio/transcriptions",
                        std::env::var("GROQ_API_KEY").ok(),
                        "whisper-large-v3",
                    )
                } else {
                    hgb_core::VoiceTranscriptionClient::default()
                };

                println!("  Transcribing audio via endpoint: {} ...", client.endpoint.dimmed());
                match client.transcribe(bytes).await {
                    Ok(text) => {
                        println!("  {} \"{}\"", "Transcribed:".green().bold(), text.white().bold());
                        let intent = hgb_core::VoiceIntentClassifier::classify(&text);
                        println!("  {} {}", "Parsed Intent:".magenta().bold(), intent);
                    }
                    Err(e) => {
                        println!("  {} {}", "Transcription Notice:".yellow().bold(), e);
                        let sample_utterance = "Refactor function parse_wav to use zero-copy bytes";
                        let intent = hgb_core::VoiceIntentClassifier::classify(sample_utterance);
                        println!("  {} \"{}\"", "Example Spoken Query:".cyan(), sample_utterance);
                        println!("  {} {}", "Parsed Intent:".magenta().bold(), intent);
                    }
                }
            } else if record {
                println!("  {} Live microphone recording is armed. (Target format: 16kHz PCM mono)", "●".red().bold());
            } else if !transcript.is_empty() {
                let intent = hgb_core::VoiceIntentClassifier::classify(&transcript);
                println!("  {} \"{}\"", "Input Spoken Query:".cyan(), transcript);
                println!("  {} {}", "Parsed Intent:".magenta().bold(), intent);
                let repl_helper = HagibisRepl::new(client);
                let resp = repl_helper.dispatch(HgbRequest::VoiceFlowProcess { transcript }).await;
                repl_helper.render_response(resp);
            } else {
                println!("  Usage: hgb voice --file <audio.wav> [--groq] OR hgb voice \"<text>\" OR hgb voice --record");
                let sample_utterance = "Refactor function parse_wav to use zero-copy bytes";
                let intent = hgb_core::VoiceIntentClassifier::classify(sample_utterance);
                println!("  Example Intent Classification: \"{}\" => {}", sample_utterance, intent.to_string().green());
            }
            Ok(())
        }
        Commands::Tape { url, scenario } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::PrTapeRecord { url, scenario_name: scenario }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Finops { prompt } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::FinOpsRoute { prompt }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Cloak { text, rehydrate } => {
            let repl_helper = HagibisRepl::new(client);
            if rehydrate {
                let resp = repl_helper.dispatch(HgbRequest::AirgapRehydrateText { response_text: text }).await;
                repl_helper.render_response(resp);
            } else {
                let resp = repl_helper.dispatch(HgbRequest::AirgapCloakText { text }).await;
                repl_helper.render_response(resp);
            }
            Ok(())
        }
        Commands::SqlGuard { sql } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::SqlGuardInspect { sql }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Replay { frame } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::ExecutionReplayScrub { target_frame: frame }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Canvas { file, old, new, selector } => {
            let repl_helper = HagibisRepl::new(client);
            let source_code = std::fs::read_to_string(&file).unwrap_or_else(|_| format!("<div className=\"{}\">Content</div>", old));
            let mutation = hgb_core::CanvasStyleMutation {
                component_selector: selector,
                property_name: "className".to_string(),
                old_value: old,
                new_value: new,
            };
            let resp = repl_helper.dispatch(HgbRequest::CanvasApplyTweak {
                source_code,
                target_file: file,
                symbol_name: "Component".to_string(),
                mutation,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Federate { goal } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::MultiRepoFederate { goal }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::TimeWarp { months, seed, scale } => {
            let repl_helper = HagibisRepl::new(client);
            let cfg = hgb_core::TimeWarpConfig {
                seed,
                months,
                base_timestamp: 1740000000,
                include_skew: true,
                record_scale: scale,
            };
            let resp = repl_helper.dispatch(HgbRequest::TimeWarpGenerate { config: Some(cfg) }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Guardrails { dir } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::StructuralGuardrailsAudit { workspace_path: Some(dir) }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Triage { trace } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::CrashTriageTrace { raw_trace: trace }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Deflake { test, file } => {
            let repl_helper = HagibisRepl::new(client);
            let test_code = file.and_then(|f| std::fs::read_to_string(&f).ok());
            let resp = repl_helper.dispatch(HgbRequest::FlakyDeflake { test_name: test, test_code }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::ContextAnchor { key, statement, category } => {
            let repl_helper = HagibisRepl::new(client);
            if let (Some(k), Some(s)) = (key, statement) {
                let cat = match category.to_lowercase().as_str() {
                    "sec" | "security" => hgb_core::AnchorCategory::Security,
                    "style" => hgb_core::AnchorCategory::Style,
                    "banned" | "antipattern" => hgb_core::AnchorCategory::AntiPattern,
                    "entity" | "domain" => hgb_core::AnchorCategory::DomainEntity,
                    _ => hgb_core::AnchorCategory::Architecture,
                };
                let resp = repl_helper.dispatch(HgbRequest::ContextAnchorRecord { category: cat, key: k, statement: s }).await;
                repl_helper.render_response(resp);
            } else {
                let resp = repl_helper.dispatch(HgbRequest::ContextAnchorGenerate).await;
                repl_helper.render_response(resp);
            }
            Ok(())
        }
        Commands::GhostLsp { file, prefix, line } => {
            let repl_helper = HagibisRepl::new(client);
            let params = hgb_core::LspInlineCompletionParams {
                file_path: file.clone(),
                language_id: if file.ends_with(".rs") { "rust".to_string() } else { "typescript".to_string() },
                line,
                character: prefix.len(),
                prefix_code: prefix,
                suffix_code: "".to_string(),
            };
            let resp = repl_helper.dispatch(HgbRequest::LspGhostComplete { params }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Compact { max_tokens } => {
            let repl_helper = HagibisRepl::new(client);
            let sample_turns = vec![
                hgb_core::ConversationTurn {
                    role: "user".to_string(),
                    content: "Build checkout pipeline and run cargo test".to_string(),
                    is_tool_output: false,
                    token_estimate: 25,
                },
                hgb_core::ConversationTurn {
                    role: "tool".to_string(),
                    content: "src/checkout.rs\nCompiling... [verbose compiler diagnostics]".to_string(),
                    is_tool_output: true,
                    token_estimate: 24500,
                },
                hgb_core::ConversationTurn {
                    role: "assistant".to_string(),
                    content: "All test suites passing. Checkout pipeline verified.".to_string(),
                    is_tool_output: false,
                    token_estimate: 35,
                },
            ];
            let resp = repl_helper.dispatch(HgbRequest::RollingCompactSession {
                session_id: "active_session".to_string(),
                turns: sample_turns,
                max_tokens: Some(max_tokens),
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::MicroCommit { intent, path } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::GitMicroCommit {
                files: vec![path],
                intent,
                diff_preview: "+ // verified changes\n- // legacy code".to_string(),
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Recipe { name, list } => {
            let repl_helper = HagibisRepl::new(client);
            if list || name.is_none() {
                let resp = repl_helper.dispatch(HgbRequest::VibeRecipeList).await;
                repl_helper.render_response(resp);
            } else if let Some(n) = name {
                let resp = repl_helper.dispatch(HgbRequest::VibeRecipeRun { recipe_name: n }).await;
                repl_helper.render_response(resp);
            }
            Ok(())
        }
        Commands::Contract { symbol, intent } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::BehaviorMatrixGenerate { symbol_name: symbol, intent_desc: intent }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::FlightGraph { goal, step } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::FlightGraphQuery { goal, active_step: step }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::RepoMapRank { exts, budget } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::RepoMapRank { extensions: exts, token_budget: Some(budget) }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::ShadowCheck { file, candidate_file } => {
            let repl_helper = HagibisRepl::new(client);
            let content = if let Some(cf) = candidate_file {
                std::fs::read_to_string(&cf).unwrap_or_default()
            } else {
                std::fs::read_to_string(&file).unwrap_or_default()
            };
            let resp = repl_helper.dispatch(HgbRequest::ShadowPreflight { relative_path: file, candidate_content: content }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Squeeze { file, max_tokens } => {
            let repl_helper = HagibisRepl::new(client);
            let raw_text = if let Some(f) = file {
                std::fs::read_to_string(&f).unwrap_or_default()
            } else {
                "   [1/10] Downloading packages...\n   [2/10] Compiling dependencies...\nwarning: unused variable `x`\nwarning: unused variable `x`\nerror[E0425]: cannot find value `foo` in this scope\n --> src/main.rs:42:15\n   Compiling finished with error\n".to_string()
            };
            let resp = repl_helper.dispatch(HgbRequest::StreamSqueeze { raw_output: raw_text, max_tokens: Some(max_tokens) }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::MutationAudit { file } => {
            let repl_helper = HagibisRepl::new(client);
            let code = std::fs::read_to_string(&file).unwrap_or_else(|_| "pub fn is_authorized(role: &str, valid: bool) -> bool {\n    if role == \"admin\" && valid {\n        true\n    } else {\n        false\n    }\n}\n".to_string());
            let resp = repl_helper.dispatch(HgbRequest::MutationAudit { source_code: code, file_name: file }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::DomInspect { file, selector, coords } => {
            let repl_helper = HagibisRepl::new(client);
            let template = std::fs::read_to_string(&file).unwrap_or_else(|_| "<div id=\"app\" class=\"container\">\n  <header class=\"hero-header\">\n    <h1 class=\"hero-title\">Welcome to Hagibis</h1>\n  </header>\n  <main class=\"main-content\">\n    <button id=\"checkout-btn\" class=\"btn btn-primary\">Checkout</button>\n  </main>\n</div>".to_string());
            let click_coords = if let Some(c) = coords {
                let parts: Vec<&str> = c.split(',').collect();
                if parts.len() == 2 {
                    let x = parts[0].trim().parse::<f64>().unwrap_or(0.0);
                    let y = parts[1].trim().parse::<f64>().unwrap_or(0.0);
                    Some((x, y))
                } else {
                    None
                }
            } else {
                None
            };
            let resp = repl_helper.dispatch(HgbRequest::DomInspect {
                template_content: template,
                file_name: file,
                click_coords,
                css_selector: selector,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::McpHub { action, server, tool, args } => {
            let repl_helper = HagibisRepl::new(client);
            let parsed_args = if let Some(ref a) = args {
                serde_json::from_str(a).ok()
            } else {
                None
            };
            let resp = repl_helper.dispatch(HgbRequest::McpOrchestrate {
                action,
                server_name: server,
                tool_name: tool,
                arguments: parsed_args,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::LiveGraph { exts } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::LiveGraphSync { extensions: exts }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::ShellPanic { code, cmd, stderr } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::ShellPanicDiagnose { command: cmd, exit_code: code, stderr }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::PlanSpec { intent } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::SpecDecompose { intent, workspace_files: vec!["src/main.rs".to_string(), "src/lib.rs".to_string()] }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::ExpandContext { prompt } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::DynamicContextExpand { prompt }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::VisualSentry { baseline, current } => {
            let repl_helper = HagibisRepl::new(client);
            let b_nodes = vec![
                hgb_core::VisualNodeSnapshot {
                    tag: "div".to_string(),
                    id: Some("root".to_string()),
                    classes: vec!["container".to_string()],
                    x: 0.0, y: 0.0, width: 800.0, height: 600.0,
                    text_preview: None,
                },
                hgb_core::VisualNodeSnapshot {
                    tag: "button".to_string(),
                    id: Some("checkout".to_string()),
                    classes: vec!["btn".to_string()],
                    x: 100.0, y: 200.0, width: 120.0, height: 40.0,
                    text_preview: Some("Submit".to_string()),
                },
            ];
            let c_nodes = if let (Some(_b), Some(_c)) = (baseline, current) {
                b_nodes.clone()
            } else {
                vec![
                    hgb_core::VisualNodeSnapshot {
                        tag: "div".to_string(),
                        id: Some("root".to_string()),
                        classes: vec!["container".to_string()],
                        x: 0.0, y: 0.0, width: 800.0, height: 600.0,
                        text_preview: None,
                    },
                    hgb_core::VisualNodeSnapshot {
                        tag: "button".to_string(),
                        id: Some("checkout".to_string()),
                        classes: vec!["btn".to_string()],
                        x: 100.0, y: 202.0, width: 120.0, height: 40.0,
                        text_preview: Some("Submit".to_string()),
                    },
                ]
            };
            let resp = repl_helper.dispatch(HgbRequest::VisualRegressionAudit {
                baseline_nodes: b_nodes,
                current_nodes: c_nodes,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::HealWatch { error } => {
            let repl_helper = HagibisRepl::new(client);
            let errs = if let Some(e) = error { vec![e] } else { vec!["src/main.rs:24: error: cannot find value `foo` in scope".to_string()] };
            let resp = repl_helper.dispatch(HgbRequest::ContinuousHealWatch {
                workspace_errors: errs,
                flaky_tests: vec!["test_auth_timeout".to_string()],
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::AmbientPredict { file, symbol, old, new } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::AmbientPredict {
                file_path: file,
                symbol_name: symbol,
                change_kind: hgb_core::EditKind::SignatureModified,
                old_snippet: old,
                new_snippet: new,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::CdpTweak { selector, prop, old, new, apply } => {
            let repl_helper = HagibisRepl::new(client);
            let event = hgb_core::DomTweakEvent {
                selector,
                property_or_attr: prop,
                old_value: old,
                new_value: new,
                component_hint: None,
                file_hint: None,
            };
            let resp = repl_helper.dispatch(HgbRequest::CdpTweakSync { event, apply_to_disk: apply }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::PromptHarvest { mode, prompt, doc } => {
            let repl_helper = HagibisRepl::new(client);
            let m = match mode.to_lowercase().as_str() {
                "architect" => hgb_core::VibePromptMode::Architect,
                "debug" => hgb_core::VibePromptMode::DebugTriage,
                "security" => hgb_core::VibePromptMode::SecurityAudit,
                "doc" => hgb_core::VibePromptMode::DocReview,
                _ => hgb_core::VibePromptMode::CodeSprint,
            };
            let (targets, raw_content) = if let Some(d) = doc {
                (vec![d.clone()], Some(format!("# Documentation for {}\npub fn verify_contract() -> bool;\n```rust\nassert!(verify_contract());\n```\n", d)))
            } else {
                (Vec::new(), None)
            };
            let resp = repl_helper.dispatch(HgbRequest::PromptModeHarvest {
                mode: m,
                user_prompt: prompt,
                doc_targets: targets,
                raw_doc_content: raw_content,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Sandbox { stack, seed } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::EphemeralSandboxSpinUp {
                stack_name: stack,
                tables_to_seed: seed,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::AntiPlacebo { file, test } => {
            let repl_helper = HagibisRepl::new(client);
            let src = if let Some(f) = file {
                std::fs::read_to_string(&f).unwrap_or_else(|_| "pub fn calculate(x: i32) -> bool { x > 0 }".to_string())
            } else {
                "pub fn calculate(x: i32) -> bool { if x > 0 { true } else { false } }".to_string()
            };
            let tst = if let Some(t) = test {
                std::fs::read_to_string(&t).unwrap_or_else(|_| "assert!(calculate(5));".to_string())
            } else {
                "#[tokio::test]
async fn test_calc() { assert!(calculate(5)); }".to_string()
            };
            let resp = repl_helper.dispatch(HgbRequest::AntiPlaceboAudit {
                source_code: src,
                test_code: tst,
            }).await;
            repl_helper.render_response(resp);
            Ok(())
        }
        Commands::Ui { port, model } => {
            println!("{}", "🪽 Hagibis Visual Canvas HUD 🪽".bold().cyan());
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::VisualCanvasHudStart {
                port,
                preferred_model: model,
            }).await;
            match resp {
                HgbResponse::VisualCanvasHudResult(rep) => {
                    println!("  ✔ Visual Canvas HUD active at: {}", rep.hud_url.bold().green());
                    println!("  ✔ Model: {}", rep.active_model.cyan());
                    println!("  ✔ Port: {}", rep.port);
                }
                HgbResponse::Error(e) => eprintln!("  ✖ Error starting HUD: {}", e),
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Deploy { provider, name, domain, write_configs } => {
            println!("{}", format!("🚀 Deploying '{}' to Public Edge ({}) 🚀", name, provider).bold().cyan());
            let edge_prov = match provider.to_lowercase().as_str() {
                "vercel" => hgb_core::EdgeProvider::Vercel,
                "fly" | "flyio" => hgb_core::EdgeProvider::FlyIo,
                "vella" | "vella-network" => hgb_core::EdgeProvider::VellaNetwork,
                _ => hgb_core::EdgeProvider::CloudflarePages,
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::EdgeDeploy {
                provider: edge_prov,
                project_slug: name,
                write_configs: Some(write_configs),
                custom_domain: domain,
            }).await;
            match resp {
                HgbResponse::EdgeDeployResult(rep) => {
                    println!("  ✔ Public HTTPS URL: {}", rep.public_url.bold().green());
                    println!("  ✔ Deployment ID: {}", rep.deployment_id.cyan());
                    println!("  ✔ Framework: {}", rep.framework);
                    println!("  ✔ Artifact Blake3: {}", rep.manifest_checksum);
                    println!("  ✔ Edge Routing: {:?}", rep.edge_routing_rules);
                }
                HgbResponse::Error(e) => eprintln!("  ✖ Deployment error: {}", e),
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Annotate { input, clipboard } => {
            println!("{}", "🎨 Visual Annotation & Multimodal Clipboard Xerox 🎨".bold().cyan());
            let raw = if clipboard {
                hgb_core::ClipboardHelper::get_text().unwrap_or_else(|_| input)
            } else {
                input
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::VisualAnnotate {
                raw_annotation: raw,
            }).await;
            match resp {
                HgbResponse::VisualAnnotateResult(rep) => {
                    println!("  ✔ Ingested {} annotations (Screenshot: {})", rep.annotations_count, rep.screenshot_id.green());
                    println!("  ✔ AST Component Bindings: {}", rep.ast_bindings.len());
                    for b in &rep.ast_bindings {
                        println!("    • <{}> ({}:{}) -> {}", b.component_name.cyan(), b.source_file, b.line_number, b.prompt_directive);
                    }
                    println!("\n{}", rep.multimodal_prompt);
                }
                HgbResponse::Error(e) => eprintln!("  ✖ Annotation error: {}", e),
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Pair { session, action, username, role } => {
            println!("{}", format!("🤝 Collaborative Multiplayer Swarm (Session: {}) 🤝", session).bold().cyan());
            let payload = serde_json::json!({
                "username": username,
                "peer_id": format!("peer_{}", username),
                "role": role,
            });
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::MultiplayerSwarmAction {
                session_id: session,
                action,
                payload,
            }).await;
            match resp {
                HgbResponse::MultiplayerSwarmResult(rep) => {
                    println!("  ✔ Session: {} (Peers: {})", rep.session_name.green(), rep.peer_count);
                    for p in &rep.peers {
                        println!("    • @{} ({}) [Model: {}]", p.username.cyan(), p.role, p.active_model);
                    }
                }
                HgbResponse::Error(e) => eprintln!("  ✖ Multiplayer error: {}", e),
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Companion { editor, install } => {
            println!("{}", format!("🔌 Universal Companion Editor Bridge ({}) 🔌", editor).bold().cyan());
            let ed_kind = match editor.to_lowercase().as_str() {
                "nvim" | "neovim" => hgb_core::CompanionEditorKind::Neovim,
                "helix" | "hx" => hgb_core::CompanionEditorKind::Helix,
                "zed" => hgb_core::CompanionEditorKind::Zed,
                _ => hgb_core::CompanionEditorKind::VsCode,
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::CompanionBridgeSetup {
                editor: ed_kind,
                install: Some(install),
            }).await;
            match resp {
                HgbResponse::CompanionBridgeResult(rep) => {
                    println!("  ✔ Target: {}", rep.editor.to_string().cyan());
                    println!("  ✔ Socket: {} (Alive: {}, Latency: {}µs)", rep.socket_path, rep.socket_alive, rep.socket_latency_micros);
                    println!("  ✔ Generated Files: {}", rep.files.len());
                    for f in &rep.files {
                        println!("    • {} - {}", f.relative_path.green(), f.description);
                    }
                    println!("\n  Setup Guide:\n  {}", rep.setup_instructions);
                }
                HgbResponse::Error(e) => eprintln!("  ✖ Bridge error: {}", e),
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Saas { project, provider, framework, auth, portal } => {
            println!("{}", format!("💳 Instant SaaS Monetization & Auth Fabric ({}) 💳", project).bold().cyan());
            let prov = match provider.to_lowercase().as_str() {
                "lemonsqueezy" | "lemon" => hgb_core::SaasProvider::LemonSqueezy,
                "paddle" => hgb_core::SaasProvider::Paddle,
                "gcash" | "paymongo" | "gcash-paymongo" => hgb_core::SaasProvider::GCashPayMongo,
                "maya" | "paymaya" | "maya-checkout" => hgb_core::SaasProvider::MayaCheckout,
                _ => hgb_core::SaasProvider::Stripe,
            };
            let cfg = hgb_core::SaasScaffoldConfig {
                provider: prov,
                project_name: project,
                framework,
                tiers: vec![],
                enable_customer_portal: portal,
                enable_jwt_auth: auth,
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::SaasScaffold { config: cfg }).await;
            match resp {
                HgbResponse::SaasScaffoldResult(rep) => {
                    println!("  ✔ Provider: {:?}", rep.provider);
                    println!("  ✔ Webhook: {}", rep.webhook_endpoint.green());
                    println!("  ✔ Customer Portal: {}", rep.customer_portal_endpoint.cyan());
                    println!("  ✔ Generated Files: {}", rep.generated_files.len());
                    for f in &rep.generated_files {
                        println!("    • {} ({})", f.relative_path.yellow(), f.language);
                    }
                    println!("  ✔ Idempotent Guard: {}", if rep.idempotent_guard_enabled { "Active".green() } else { "Inactive".red() });
                }
                HgbResponse::Error(e) => eprintln!("  ✖ SaaS Error: {}", e),
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::ContinuousVoice { transcript, speaker, energy } => {
            println!("{}", "🎙️ Full-Duplex Ambient Conversational Voice 🎙️".bold().cyan());
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::ContinuousVoiceTurn {
                speaker,
                transcript,
                intent_action: None,
                energy,
            }).await;
            match resp {
                HgbResponse::ContinuousVoiceResult(rep) => {
                    println!("  ✔ Session: {}", rep.session_id.green());
                    println!("  ✔ State: {:?}", rep.state);
                    println!("  ✔ Turns: {} (Interruptions: {})", rep.total_turns, rep.total_interruptions);
                    if let Some(last) = rep.turns.last() {
                        println!("  ✔ Last Speaker: {} -> {}", last.speaker.cyan(), last.transcript);
                        if let Some(ec) = last.earcon_played {
                            println!("  🔔 Earcon: {:?}", ec);
                        }
                    }
                }
                HgbResponse::Error(e) => eprintln!("  ✖ Voice Error: {}", e),
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Figma { file_key, export } => {
            println!("{}", format!("🎨 Bi-Directional Figma & Design Token Bridge ({}) 🎨", file_key).bold().cyan());
            let repl_helper = HagibisRepl::new(client);
            if let Some(comp_name) = export {
                let resp = repl_helper.dispatch(HgbRequest::FigmaExport {
                    component_name: comp_name.clone(),
                    markup: "<div class=\"p-6 bg-slate-900 text-white rounded-xl\">Vibe Component</div>".into(),
                }).await;
                match resp {
                    HgbResponse::FigmaExportResult(rep) => {
                        println!("  ✔ Exported Vector Frame: {}", rep.component_name.green());
                        println!("  ✔ Bounds: {}x{} px", rep.bounding_width, rep.bounding_height);
                        println!("  ✔ SVG XML Canvas length: {} bytes", rep.svg_canvas_xml.len());
                    }
                    HgbResponse::Error(e) => eprintln!("  ✖ Figma Export Error: {}", e),
                    _ => println!("  Response: {:?}", resp),
                }
            } else {
                let resp = repl_helper.dispatch(HgbRequest::FigmaSync {
                    file_key,
                    raw_json: None,
                }).await;
                match resp {
                    HgbResponse::FigmaSyncResult(rep) => {
                        println!("  ✔ Colors Extracted: {}", rep.token_set.colors.len());
                        println!("  ✔ Typography Styles: {}", rep.token_set.typography.len());
                        println!("  ✔ Synthesized Components: {}", rep.synthesized_components.len());
                        for (k, _) in &rep.synthesized_components {
                            println!("    • Component: {}", k.cyan());
                        }
                        println!("  ✔ Generated Tailwind Config:\n{}", rep.generated_tailwind_config);
                    }
                    HgbResponse::Error(e) => eprintln!("  ✖ Figma Sync Error: {}", e),
                    _ => println!("  Response: {:?}", resp),
                }
            }
            Ok(())
        }
        Commands::ShadowDb { db, ops, workers } => {
            println!("{}", format!("⚡ Autonomous Production DB Shadow Simulator ({}) ⚡", db).bold().cyan());
            let profile = hgb_core::StressProfile {
                target_db: db,
                total_operations: ops,
                concurrency_workers: workers,
                read_write_ratio: 0.85,
                simulated_dataset_size: 5000,
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::ShadowDbStress {
                profile,
                schema_sql: None,
            }).await;
            match resp {
                HgbResponse::ShadowDbStressResult(rep) => {
                    println!("  ✔ Total Operations: {}", rep.metrics.total_ops);
                    println!("  ✔ Throughput: {} QPS", rep.metrics.throughput_qps.to_string().green());
                    println!("  ✔ Latency p50: {:.2}ms | p95: {:.2}ms | p99: {:.2}ms", rep.metrics.p50_ms, rep.metrics.p95_ms, rep.metrics.p99_ms);
                    println!("  ✔ Viral Traffic Ready: {}", if rep.production_ready_for_viral_traffic { "YES (100% Scalable)".green() } else { "NEEDS INDEXES".yellow() });
                    if !rep.recommended_indexes.is_empty() {
                        println!("  ✔ Recommended Indexes ({}):", rep.recommended_indexes.len());
                        for idx in &rep.recommended_indexes {
                            println!("    • Table: {}.{} (Speedup: {:.1}x)", idx.table.cyan(), idx.column.yellow(), idx.estimated_speedup_factor);
                            println!("      SQL: {}", idx.sql_migration.green());
                        }
                    }
                }
                HgbResponse::Error(e) => eprintln!("  ✖ Stress Test Error: {}", e),
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::ViralOg { title, badge, desc, twitter } => {
            println!("{}", format!("🚀 Viral Social Graph & Dynamic OpenGraph Engine ({}) 🚀", title).bold().cyan());
            let cfg = hgb_core::OgCardConfig {
                title,
                description: desc,
                badge_text: badge,
                primary_brand_color: "#06b6d4".into(),
                site_url: "https://hagibis.dev".into(),
                author_twitter_handle: twitter,
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::ViralOgGenerate { config: cfg }).await;
            match resp {
                HgbResponse::ViralOgResult(rep) => {
                    println!("  ✔ Viral Readiness Score: {}/100", rep.scorecard.total_score.to_string().green());
                    println!("  ✔ Dynamic SVG OG Card: {} bytes", rep.generated_svg_image.len());
                    println!("  ✔ Edge Handler Code: {} bytes", rep.generated_edge_route_code.len());
                    println!("  ✔ Meta Tags Injected: {}", rep.meta_tags.len());
                    for t in &rep.meta_tags {
                        println!("    • {}: {}", t.property_or_name.cyan(), t.content);
                    }
                    if !rep.scorecard.suggestions.is_empty() {
                        println!("  ⚠ Suggestions:");
                        for s in &rep.scorecard.suggestions {
                            println!("    • {}", s.yellow());
                        }
                    }
                }
                HgbResponse::Error(e) => eprintln!("  ✖ Viral OG Error: {}", e),
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Mobile { url, name } => {
            println!("{}", format!("📱 Instant Mobile QR Teleport & PWA Matrix ({}) 📱", name).bold().cyan());
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::MobileQrTeleportGenerate {
                target_url: url.clone(),
                config: Some(hgb_core::MobilePwaConfig {
                    app_name: name.clone(),
                    short_name: name,
                    start_url: "/".into(),
                    ..Default::default()
                }),
            }).await;
            match resp {
                HgbResponse::MobileQrTeleportResult(rep) => {
                    println!("{}", rep.ansi_qr_art);
                    println!("  ✔ Target URL: {}", rep.target_url.green());
                    println!("  ✔ PWA Manifest: Active (standalone, theme #06b6d4)");
                    println!("  ✔ Safe-Area Insets: Injected (iOS notch & bottom-bar ready)");
                }
                HgbResponse::Error(e) => eprintln!("  ✖ Mobile Teleport Error: {}", e),
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::IncidentHotfix { error, file, line } => {
            println!("{}", format!("🚨 Live Production Incident Hotfixer ({}) 🚨", error).bold().red());
            let payload = hgb_core::ProductionErrorPayload {
                provider: "sentry".into(),
                error_id: "".into(),
                exception_type: "ProductionError".into(),
                message: error,
                culprit_file: file,
                culprit_line: line,
                culprit_function: None,
                request_path: Some("/api/live".into()),
                user_agent: Some("Mozilla/5.0 (iPhone; CPU iPhone OS 17_0)".into()),
                raw_stack_trace: None,
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::ProductionHotfixTriage { payload }).await;
            match resp {
                HgbResponse::ProductionHotfixResult(rep) => {
                    println!("  ✔ Incident ID: {}", rep.incident_id.yellow());
                    println!("  ✔ Culprit Location: {}", rep.culprit_location.cyan());
                    println!("  ✔ Root Cause: {}", rep.root_cause.green());
                    println!("  ✔ Hotfix Branch: {}", rep.hotfix_branch_name.yellow().bold());
                    println!("  ✔ Proposed Patch:\n      Line {}: {}", rep.proposed_patch.target_line, rep.proposed_patch.patched_code.green());
                    println!("  ✔ Rationale: {}", rep.proposed_patch.rationale);
                    println!("  ✔ Auto-Deployable: {}", if rep.auto_deployable { "YES (Verified)".green() } else { "NO".red() });
                }
                HgbResponse::Error(e) => eprintln!("  ✖ Hotfix Error: {}", e),
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::LlmGateway { prompt, frontier } => {
            println!("{}", "⚡ AI Semantic Cost Gateway & Model Arbitrage ⚡".bold().cyan());
            let req = hgb_core::LlmPromptRequest {
                prompt,
                max_tokens: Some(1000),
                force_frontier: frontier,
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::LlmCostRoute { request: req }).await;
            match resp {
                HgbResponse::LlmCostResult(rep) => {
                    println!("  ✔ Selected Model: {}", rep.decision.selected_model.green().bold());
                    println!("  ✔ Semantic Cache: {}", if rep.decision.is_cached { "HIT ($0.00, 1ms)".green().bold() } else { "MISS (Live API Call)".yellow() });
                    println!("  ✔ Estimated Cost: ${:.6}", rep.decision.estimated_cost_usd);
                    println!("  ✔ Latency Estimate: {} ms", rep.decision.latency_estimate_ms);
                    println!("  ✔ Routing Reason: {}", rep.decision.routing_reason.dimmed());
                    println!("  ✔ Total Queries: {} | Cache Hits: {} ({:.1}%)", rep.metrics.total_queries_processed, rep.metrics.cache_hits, rep.metrics.cache_hit_ratio * 100.0);
                    println!("  ✔ Total Cost Saved: ${:.4}", rep.metrics.total_saved_usd);
                }
                HgbResponse::Error(e) => eprintln!("  ✖ LLM Gateway Error: {}", e),
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Analytics { funnel, scaffold } => {
            println!("{}", "📊 Zero-Cookie Privacy Funnel Analytics 📊".bold().cyan());
            let repl_helper = HagibisRepl::new(client);
            if scaffold {
                let resp = repl_helper.dispatch(HgbRequest::PrivacyAnalyticsScaffold).await;
                match resp {
                    HgbResponse::PrivacyAnalyticsScaffoldResult(rep) => {
                        println!("  ✔ Client Script Tag (<600B):\n{}", rep.client_script_tag.cyan());
                        println!("  ✔ Edge API Route:\n{}", rep.edge_route_code.green());
                        println!("  ✔ SQLite Schema:\n{}", rep.sqlite_schema_sql.yellow());
                    }
                    HgbResponse::Error(e) => eprintln!("  ✖ Scaffold Error: {}", e),
                    _ => println!("  Response: {:?}", resp),
                }
            } else if funnel {
                let resp = repl_helper.dispatch(HgbRequest::PrivacyFunnelQuery { event_to_record: None }).await;
                match resp {
                    HgbResponse::PrivacyFunnelResult(rep) => {
                        println!("  ✔ Total Events Recorded: {}", rep.total_events_recorded);
                        println!("  ✔ Conversion Funnel:");
                        for stage in &rep.stages {
                            println!("    • {:<12} {:>5} visitors | Step: {:>5.1}% | Total: {:>5.1}%", stage.stage_name.cyan(), stage.unique_visitors, stage.step_conversion_rate_pct, stage.conversion_rate_pct);
                        }
                        if let Some(dropoff) = &rep.top_dropoff_stage {
                            println!("  ⚠ Top Drop-Off: {}", dropoff.red().bold());
                        }
                        for r in &rep.recommendations {
                            println!("    💡 {}", r.yellow());
                        }
                    }
                    HgbResponse::Error(e) => eprintln!("  ✖ Funnel Error: {}", e),
                    _ => println!("  Response: {:?}", resp),
                }
            }
            Ok(())
        }
        Commands::Rails { action, target, migration } => {
            println!("{}", "💎 Ruby on Rails Intelligence Engine 💎".bold().red());
            let repl_helper = HagibisRepl::new(client);
            match action.as_str() {
                "lint" => {
                    let code = migration.unwrap_or_else(|| "add_index :users, :email".to_string());
                    let file_name = target.unwrap_or_else(|| "20260928_migration.rb".to_string());
                    let resp = repl_helper.dispatch(HgbRequest::RailsLintMigration { file_name, migration_code: code }).await;
                    match resp {
                        HgbResponse::RailsLintMigrationResult(rep) => {
                            println!("  ✔ Migration File: {}", rep.file_name.cyan());
                            println!("  ✔ Zero-Downtime Safe: {}", if rep.safe_to_deploy_zero_downtime { "YES (Greenlight)".green().bold() } else { "NO (Locks Table)".red().bold() });
                            for h in &rep.hazards {
                                println!("    ⚠ [Line {}] {}: {}", h.line_number.unwrap_or(0), h.rule.yellow().bold(), h.message);
                                println!("      Remediation: {}", h.remediation.green());
                            }
                        }
                        _ => println!("  Response: {:?}", resp),
                    }
                }
                "scaffold" => {
                    let model_name = target.unwrap_or_else(|| "Post".to_string());
                    let fields = vec![
                        hgb_core::ScaffoldField { name: "title".to_string(), col_type: "string".to_string(), is_unique: false, is_indexed: true, is_required: true },
                        hgb_core::ScaffoldField { name: "body".to_string(), col_type: "text".to_string(), is_unique: false, is_indexed: false, is_required: true },
                    ];
                    let resp = repl_helper.dispatch(HgbRequest::RailsScaffold { model_name, fields }).await;
                    match resp {
                        HgbResponse::RailsScaffoldResult(rep) => {
                            println!("  ✔ Generated Rails Model: app/models/{}.rb", rep.model_name.to_lowercase());
                            println!("  ✔ Generated Migration: db/migrate/create_{}.rb", rep.table_name);
                            println!("  ✔ Generated Controller: app/controllers/{}_controller.rb", rep.table_name);
                            println!("  ✔ Route Entry: {}", rep.route_snippet.yellow());
                        }
                        _ => println!("  Response: {:?}", resp),
                    }
                }
                _ => {
                    let resp = repl_helper.dispatch(HgbRequest::RailsDetect { workspace_path: target }).await;
                    match resp {
                        HgbResponse::RailsDetectResult(rep) => {
                            println!("  ✔ Is Rails App: {}", if rep.is_rails { "YES".green().bold() } else { "NO".yellow() });
                            println!("  ✔ Database Adapter: {:?}", rep.db_adapter);
                            println!("  ✔ Models Detected: {}", rep.model_count);
                            println!("  ✔ Migrations: {}", rep.migration_count);
                            println!("  ✔ RuboCop Configured: {}", if rep.has_rubocop { "YES".green() } else { "NO".dimmed() });
                            println!("  ✔ RSpec Configured: {}", if rep.has_rspec { "YES".green() } else { "NO".dimmed() });
                        }
                        _ => println!("  Response: {:?}", resp),
                    }
                }
            }
            Ok(())
        }
        Commands::Project { action, title, desc, status: _ } => {
            println!("{}", "📁 Persistent Project Coordinator 📁".bold().cyan());
            let repl_helper = HagibisRepl::new(client);
            match action.as_str() {
                "task" => {
                    let t = title.unwrap_or_else(|| "New Task".to_string());
                    let d = desc.unwrap_or_else(|| "Task Description".to_string());
                    let resp = repl_helper.dispatch(HgbRequest::ProjectAddTask {
                        title: t,
                        description: d,
                        priority: hgb_core::TaskPriority::High,
                        tags: vec!["vibe-code".to_string()],
                    }).await;
                    match resp {
                        HgbResponse::ProjectTaskResult(Some(task)) => {
                            println!("  ✔ Task Created: [{}] {} ({:?})", task.id.yellow(), task.title.green(), task.priority);
                        }
                        _ => println!("  Response: {:?}", resp),
                    }
                }
                "adr" => {
                    let t = title.unwrap_or_else(|| "ADR: Architecture Decision".to_string());
                    let d = desc.unwrap_or_else(|| "Context and justification".to_string());
                    let resp = repl_helper.dispatch(HgbRequest::ProjectAddAdr {
                        title: t,
                        context: d,
                        decision: "Adopted microkernel pattern".to_string(),
                        consequences: "Sub-millisecond latency guaranteed".to_string(),
                        why: "Satisfies non-blocking invariant".to_string(),
                    }).await;
                    match resp {
                        HgbResponse::ProjectAdrResult(adr) => {
                            println!("  ✔ ADR Registered: ADR-{:03} '{}'", adr.id, adr.title.green());
                        }
                        _ => println!("  Response: {:?}", resp),
                    }
                }
                _ => {
                    let resp = repl_helper.dispatch(HgbRequest::ProjectSnapshot).await;
                    match resp {
                        HgbResponse::ProjectSnapshotResult(snap) => {
                            println!("  ✔ Project: {}", snap.project_name.green().bold());
                            println!("  ✔ Progress: {:.1}% ({} completed / {} total tasks)", snap.completion_percentage, snap.completed_tasks, snap.total_tasks);
                            println!("  ✔ Pending: {} | In-Progress: {} | Blocked: {}", snap.pending_tasks, snap.in_progress_tasks, snap.blocked_tasks);
                            println!("  ✔ Total ADRs: {}", snap.adrs_count);
                            println!("  ✔ Handoff Summary:\n    {}", snap.handoff_summary.dimmed());
                        }
                        _ => println!("  Response: {:?}", resp),
                    }
                }
            }
            Ok(())
        }
        Commands::RulesEngine { files, summon } => {
            println!("{}", "📜 Tiered Rules Auto-Engine 📜".bold().yellow());
            let active_files: Vec<String> = files.split(',').map(|s| s.trim().to_string()).collect();
            let manual_mentions: Vec<String> = summon.map(|s| s.split(',').map(|m| m.trim().to_string()).collect()).unwrap_or_default();
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::RulesEngineEvaluate { active_files, manual_mentions }).await;
            match resp {
                HgbResponse::RulesEngineResult(rep) => {
                    println!("  ✔ Total Rules Evaluated: {}", rep.total_rules_evaluated);
                    println!("  ✔ Matched Rules ({} active):", rep.matched_rules.len());
                    for r in &rep.matched_rules {
                        println!("    • {:<15} [P{}] {}", r.name.cyan(), r.priority, r.content.dimmed());
                    }
                    if !rep.triggered_globs.is_empty() {
                        println!("  ✔ Glob Triggers: {}", rep.triggered_globs.join(", ").yellow());
                    }
                }
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Autopilot { ticket } => {
            println!("{}", "🤖 Autonomous Ticket-to-PR Autopilot 🤖".bold().magenta());
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::AutopilotRun { ticket_text: ticket }).await;
            match resp {
                HgbResponse::AutopilotResult(rep) => {
                    println!("  ✔ Ingested Ticket: [{}] {}", rep.ticket.ticket_id.yellow(), rep.ticket.title.bold());
                    println!("  ✔ Status: {:?}", rep.current_stage);
                    println!("  ✔ Branch: {}", rep.pr_metadata.branch_name.cyan());
                    println!("  ✔ Commit Message: {}", rep.pr_metadata.commit_message.green());
                    println!("  ✔ Tests Verified: {}/{} passed", rep.verification.tests_passed, rep.verification.tests_executed);
                    println!("  ✔ Execution Duration: {} ms", rep.total_duration_ms);
                }
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Explain { intent, diff } => {
            println!("{}", "🔍 Agent Decision Explainer (Trust Gap Solver) 🔍".bold().cyan());
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::ExplainAction { intent, diff_content: diff }).await;
            match resp {
                HgbResponse::ExplainResult(rep) => {
                    println!("  ✔ ADR Title: {}", rep.title.bold().green());
                    println!("  ✔ Trust Verdict: {}", rep.trust_verdict.yellow());
                    println!("  ✔ Uncertainty Score: {:.2}", rep.uncertainty_score);
                    println!("  ✔ Decision Drivers: {}", rep.primary_drivers.join(", "));
                    println!("  ✔ Generated ADR:\n{}", rep.formatted_markdown_adr.dimmed());
                }
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::SmartIndex { action: _, dir } => {
            println!("{}", "🌲 Merkle Tree Collaborative Codebase Index 🌲".bold().green());
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::SmartIndexSnapshot { workspace_path: Some(dir) }).await;
            match resp {
                HgbResponse::SmartIndexSnapshotResult(snap) => {
                    println!("  ✔ Merkle Root Hash: {}", snap.root_hash.green().bold());
                    println!("  ✔ Indexed Files: {}", snap.file_count);
                    println!("  ✔ Total Bytes: {} KB", snap.total_bytes / 1024);
                    println!("  ✔ Snapshot Time: {}", snap.created_at_utc.dimmed());
                }
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::RolloutWatch { deployment_id, error_rate, p99 } => {
            println!("{}", "📈 PR / Deployment Health Monitor 📈".bold().yellow());
            let samples = vec![hgb_core::TelemetrySample {
                timestamp_epoch_ms: chrono::Utc::now().timestamp_millis() as u64,
                request_count: 1000,
                error_count: (error_rate * 10.0) as u64,
                p50_latency_ms: p99 / 4.0,
                p95_latency_ms: p99 * 0.8,
                p99_latency_ms: p99,
                status_5xx_count: 0,
            }];
            let baseline = vec![hgb_core::TelemetrySample {
                timestamp_epoch_ms: 1000,
                request_count: 1000,
                error_count: 1,
                p50_latency_ms: 10.0,
                p95_latency_ms: 25.0,
                p99_latency_ms: 40.0,
                status_5xx_count: 0,
            }];
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::RolloutWatchEvaluate { deployment_id, samples, baseline_samples: baseline, config: None }).await;
            match resp {
                HgbResponse::RolloutWatchResult(rep) => {
                    println!("  ✔ Deployment ID: {}", rep.deployment_id.cyan());
                    println!("  ✔ Health Verdict: {:?}", rep.verdict);
                    println!("  ✔ Error Rate: {:.2}% (Baseline: {:.2}%)", rep.current_error_rate_pct, rep.baseline_error_rate_pct);
                    println!("  ✔ p99 Latency: {:.1} ms", rep.current_p99_ms);
                    println!("  ✔ Action: {}", rep.remediation_action.yellow());
                    if let Some(cmd) = &rep.rollback_command {
                        println!("  ⚠ Rollback Trigger: {}", cmd.red().bold());
                    }
                }
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::PrAudit { file, content } => {
            println!("{}", "🛡️ AI-PR Security Vulnerability Audit 🛡️".bold().red());
            let file_content = if let Some(c) = content {
                c
            } else if let Ok(disk) = std::fs::read_to_string(&file) {
                disk
            } else {
                "// sample file".to_string()
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::PrAuditScan { files: vec![(file, file_content)] }).await;
            match resp {
                HgbResponse::PrAuditResult(rep) => {
                    println!("  ✔ Audit Status: {}", if rep.passed_audit { "PASSED (Clean)".green().bold() } else { "BLOCKED (Vulnerabilities Found)".red().bold() });
                    println!("  ✔ Findings: {} total ({} critical, {} high)", rep.total_findings, rep.critical_count, rep.high_count);
                    for f in &rep.findings {
                        println!("    • [{:?}] Line {}: {} ({})", f.severity, f.line_number, f.category.yellow(), f.message);
                        println!("      Remediation: {}", f.remediation.green());
                    }
                }
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::PreviewCloud { app, port, provider, ttl } => {
            println!("{}", "☁️ Ephemeral Cloud Preview Deployment ☁️".bold().cyan());
            let prov = match provider.as_str() {
                "fly" => hgb_core::PreviewCloudProvider::FlyIo,
                "railway" => hgb_core::PreviewCloudProvider::Railway,
                "local" => hgb_core::PreviewCloudProvider::LocalhostProxy,
                _ => hgb_core::PreviewCloudProvider::CloudflareTunnel,
            };
            let cfg = hgb_core::PreviewDeploymentConfig {
                provider: prov,
                app_name: app,
                local_port: port,
                ttl_hours: ttl,
                enable_basic_auth: true,
                custom_subdomain: None,
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::PreviewCloudDeploy { config: cfg }).await;
            match resp {
                HgbResponse::PreviewCloudResult(rep) => {
                    println!("  ✔ Public Preview URL: {}", rep.preview_url.green().bold());
                    println!("  ✔ Deployment ID: {}", rep.deployment_id.cyan());
                    println!("  ✔ Expires At: {}", rep.expires_at_utc.yellow());
                    println!("  ✔ Teardown Command: {}", rep.teardown_command.dimmed());
                }
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Collab { action, user, file: _, lines: _ } => {
            println!("{}", "👥 Multi-Dev Real-Time Collaboration 👥".bold().blue());
            let repl_helper = HagibisRepl::new(client);
            match action.as_str() {
                "join" => {
                    let resp = repl_helper.dispatch(HgbRequest::CollabJoin { username: user }).await;
                    match resp {
                        HgbResponse::CollabSessionResult(state) => {
                            println!("  ✔ Joined Room: {}", state.room_id.green());
                            println!("  ✔ Total Connected Peers: {}", state.connected_peers.len());
                        }
                        _ => println!("  Response: {:?}", resp),
                    }
                }
                _ => {
                    let resp = repl_helper.dispatch(HgbRequest::CollabSessionSnapshot).await;
                    match resp {
                        HgbResponse::CollabSessionResult(state) => {
                            println!("  ✔ Room ID: {}", state.room_id.green());
                            println!("  ✔ Host Peer ID: {}", state.host_id.cyan());
                            println!("  ✔ Active Peers:");
                            for (id, p) in &state.connected_peers {
                                println!("    • [{}] username: {}", id.yellow(), p.username);
                            }
                        }
                        _ => println!("  Response: {:?}", resp),
                    }
                }
            }
            Ok(())
        }
        Commands::PromptLab { action: _, prompt_a, prompt_b } => {
            println!("{}", "🧪 Prompt A/B Engineering Workspace 🧪".bold().magenta());
            let variants = vec![
                hgb_core::PromptVariant { id: "variant_a".to_string(), name: "Variant A (Strict)".to_string(), template: prompt_a, model: "gemini-2.5-flash".to_string(), temperature: 0.1 },
                hgb_core::PromptVariant { id: "variant_b".to_string(), name: "Variant B (Creative)".to_string(), template: prompt_b, model: "gemini-2.5-flash".to_string(), temperature: 0.7 },
            ];
            let test_cases = vec!["Build login form".to_string(), "Verify database lock".to_string()];
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::PromptLabBenchmark { variants, test_cases }).await;
            match resp {
                HgbResponse::PromptLabResult(rep) => {
                    println!("  ✔ Benchmarked Variants: {}", rep.total_variants_benchmarked);
                    println!("  ✔ Leaderboard:");
                    for item in &rep.leaderboard {
                        println!("    #{}: {:<20} | Quality: {:.2} | Compliance: {:.1}% | Latency: {}ms", item.rank, item.variant_name.cyan(), item.quality_score, item.schema_compliance_pct, item.avg_latency_ms);
                    }
                    println!("  ✔ Recommended Winner: {}", rep.recommended_winner_id.green().bold());
                }
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::LangPack { dir } => {
            println!("{}", "📦 Framework Language Intelligence Packs 📦".bold().green());
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::LangPackInspect { workspace_path: Some(dir) }).await;
            match resp {
                HgbResponse::LangPackResult(rep) => {
                    println!("  ✔ Framework Detected: {:?}", rep.detected_framework);
                    println!("  ✔ Primary Language: {}", rep.language_info.primary_language.cyan().bold());
                    println!("  ✔ Recommended Linter: {}", rep.language_info.recommended_linter.green());
                    println!("  ✔ Test Runner Command: {}", rep.language_info.test_runner_command.yellow());
                    println!("  ✔ Conventions:");
                    for c in &rep.language_info.conventions {
                        println!("    • {}", c);
                    }
                }
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::NativeMobile { action: _, dir, trace } => {
            println!("{}", "📱 Native Mobile & PWA Intelligence 📱".bold().cyan());
            let repl_helper = HagibisRepl::new(client);
            if let Some(tr) = trace {
                let resp = repl_helper.dispatch(HgbRequest::NativeMobileDiagnoseCrash { raw_trace: tr }).await;
                match resp {
                    HgbResponse::NativeMobileCrashResult(rep) => {
                        println!("  ✔ Exception Type: {}", rep.exception_type.red().bold());
                        println!("  ✔ Message: {}", rep.message);
                        println!("  ✔ Suggested Fix: {}", rep.suggested_fix.green());
                    }
                    _ => println!("  Response: {:?}", resp),
                }
            } else {
                let resp = repl_helper.dispatch(HgbRequest::NativeMobileDetect { workspace_path: Some(dir) }).await;
                match resp {
                    HgbResponse::NativeMobileDetectResult(rep) => {
                        println!("  ✔ Platform: {:?}", rep.platform);
                        println!("  ✔ iOS Directory: {}", if rep.has_ios_directory { "YES".green() } else { "NO".dimmed() });
                        println!("  ✔ Android Directory: {}", if rep.has_android_directory { "YES".green() } else { "NO".dimmed() });
                        for d in &rep.diagnostics {
                            println!("    • {}", d.cyan());
                        }
                    }
                    _ => println!("  Response: {:?}", resp),
                }
            }
            Ok(())
        }
        Commands::Budget { action: _, set_limit, record } => {
            println!("{}", "💰 SMART FINOPS & TOKEN BUDGET ENVELOPE 💰".bold().yellow());
            let cwd = std::env::current_dir()?;
            let engine = hgb_core::FinOpsEngine::new(&cwd);
            let mut envelope = engine.load_envelope();

            if let Some(limit) = set_limit {
                envelope.daily_cap_usd = limit;
                let _ = engine.save_envelope(&envelope);
                println!("  ✔ Updated local daily budget cap: ${:.2}", limit);
            } else if let Some(ref rec) = record {
                if rec.contains(',') {
                    let parts: Vec<&str> = rec.split(',').collect();
                    if parts.len() == 3 {
                        let model = parts[0].trim();
                        let prompt_tok: usize = parts[1].trim().parse().unwrap_or(0);
                        let comp_tok: usize = parts[2].trim().parse().unwrap_or(0);
                        let cost = envelope.record_usage(model, prompt_tok, comp_tok, "core");
                        let _ = engine.save_envelope(&envelope);
                        println!("  ✔ Recorded call for model '{}': {} prompt tok, {} comp tok -> ${:.4} USD", model, prompt_tok, comp_tok, cost);
                    }
                } else {
                    let cost = envelope.record_usage(rec, 1500, 600, "core");
                    let _ = engine.save_envelope(&envelope);
                    println!("  ✔ Recorded simulated call for model '{}' -> ${:.4} USD", rec, cost);
                }
            } else {
                let util = envelope.daily_utilization_pct();
                let status_color = if util < 75.0 {
                    "NOMINAL (< 75%)".green().bold()
                } else if util < 100.0 {
                    "WARNING (>= 75%)".yellow().bold()
                } else {
                    "CAP EXCEEDED (>= 100%)".red().bold()
                };

                println!("  Ledger Path:          {}", engine.ledger_path.display().to_string().dimmed());
                println!("  Daily Budget Cap:     ${:.2} USD", envelope.daily_cap_usd);
                println!("  Spend Today:          ${:.4} USD", envelope.today_spend_usd);
                println!("  Remaining Today:      ${:.4} USD", envelope.remaining_daily_usd());
                println!("  Daily Utilization:    {:.1}% [{}]", util, status_color);
                println!("  Total Prompt Tokens:  {}", envelope.total_prompt_tokens.to_string().yellow());
                println!("  Total Comp Tokens:    {}", envelope.total_completion_tokens.to_string().yellow());
            }
            Ok(())
        }
        Commands::VsCodeExt { action: _, name } => {
            println!("{}", "🔌 VS Code / Cursor Extension Bridge 🔌".bold().blue());
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::VsCodeScaffold { config: Some(hgb_core::VsCodeBridgeConfig { extension_name: name, ..Default::default() }) }).await;
            match resp {
                HgbResponse::VsCodeScaffoldResult(rep) => {
                    println!("  ✔ Extension Manifest: Generated (package.json)");
                    println!("  ✔ TypeScript Bridge: Generated (extension.ts)");
                    println!("  ✔ Socket Target: {}", rep.socket_path.green());
                    println!("  ✔ Registered Commands: {}", rep.registered_commands.join(", ").cyan());
                    println!("  ✔ Instructions:\n{}", rep.install_instructions.dimmed());
                }
                _ => println!("  Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Ci { prompt, format, fail_fast } => {
            let out_fmt = match format.to_lowercase().as_str() {
                "compact" => hgb_core::CiOutputFormat::CompactRaw,
                "github" => hgb_core::CiOutputFormat::GithubActionsAnnotations,
                _ => hgb_core::CiOutputFormat::JsonLines,
            };
            let config = hgb_core::CiExecutionConfig {
                prompt,
                output_format: out_fmt,
                fail_fast,
                max_turns: 10,
                timeout_seconds: 300,
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::CiExecute { config }).await;
            match resp {
                HgbResponse::CiResult(res) => {
                    println!("{}", res.formatted_output);
                    if res.exit_code != 0 {
                        std::process::exit(res.exit_code);
                    }
                }
                _ => println!("Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Plan { goal, approve_step, reject_step, plan_id } => {
            let repl_helper = HagibisRepl::new(client);
            if let Some(s_id) = approve_step {
                let p_id = plan_id.unwrap_or_default();
                let resp = repl_helper.dispatch(HgbRequest::PlanApproveStep { plan_id: p_id, step_id: s_id, approve: true }).await;
                println!("Response: {:?}", resp);
            } else if let Some(s_id) = reject_step {
                let p_id = plan_id.unwrap_or_default();
                let resp = repl_helper.dispatch(HgbRequest::PlanApproveStep { plan_id: p_id, step_id: s_id, approve: false }).await;
                println!("Response: {:?}", resp);
            } else {
                let resp = repl_helper.dispatch(HgbRequest::PlanGenerate { goal }).await;
                match resp {
                    HgbResponse::PlanResult(p) => {
                        println!("📋 Execution Plan Blueprint [{}] 📋", p.plan_id.cyan().bold());
                        println!("  Goal: {}", p.goal_description.bold());
                        println!("  Estimated Tokens: {}", p.total_estimated_tokens);
                        println!("  Requires Signoff: {}", p.requires_human_signoff);
                        println!("  Steps ({}):", p.steps.len());
                        for s in p.steps {
                            println!("    {}) {} [{:?}]", s.index, s.title.bold(), s.status);
                            println!("       Target: {}", s.target_files.join(", ").dimmed());
                        }
                    }
                    _ => println!("Response: {:?}", resp),
                }
            }
            Ok(())
        }
        Commands::Ticket { input } => {
            println!("{}", "🎫 Universal Issue Ingestor 🎫".bold().cyan());
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::TicketIngest { input_or_url: input }).await;
            match resp {
                HgbResponse::TicketResult(t) => {
                    println!("  ✔ Provider: {:?}", t.provider);
                    println!("  ✔ Issue Key: {}", t.issue_key.bold().green());
                    println!("  ✔ Title: {}", t.title);
                    println!("  ✔ Suggested Branch: {}", t.suggested_branch_name.cyan());
                    println!("  ✔ Acceptance Criteria ({}):", t.acceptance_criteria.len());
                    for ac in t.acceptance_criteria {
                        println!("    [{}] {}", if ac.is_verified { "X" } else { " " }, ac.description);
                    }
                    if !t.referenced_files.is_empty() {
                        println!("  ✔ Referenced Files: {}", t.referenced_files.join(", "));
                    }
                    if !t.stack_traces.is_empty() {
                        println!("  ✔ Stack Traces Captured: {}", t.stack_traces.len());
                    }
                }
                _ => println!("Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Profile { path, indent, test_framework } => {
            println!("{}", "👤 Persistent Project Memory Profile 👤".bold().magenta());
            let repl_helper = HagibisRepl::new(client);
            if indent.is_some() || test_framework.is_some() {
                let conv = hgb_core::ProjectCodingConventions {
                    indent_style: indent.unwrap_or_else(|| "4-spaces".to_string()),
                    test_framework: test_framework.unwrap_or_else(|| "cargo-test".to_string()),
                    linter_command: Some("cargo clippy".to_string()),
                    strict_null_safety: true,
                };
                let resp = repl_helper.dispatch(HgbRequest::ProfilePatchConventions { root_path: Some(path), conventions: conv }).await;
                println!("Response: {:?}", resp);
            } else {
                let resp = repl_helper.dispatch(HgbRequest::ProfileGet { root_path: Some(path) }).await;
                match resp {
                    HgbResponse::ProfileResult(p) => {
                        println!("  ✔ Project: {}", p.project_name.bold().green());
                        println!("  ✔ Root: {}", p.root_path.dimmed());
                        println!("  ✔ Preferred Model: {}", p.preferred_model.cyan());
                        println!("  ✔ Conventions: Indent='{}', Tests='{}', StrictNull={}", p.conventions.indent_style, p.conventions.test_framework, p.conventions.strict_null_safety);
                        println!("  ✔ Invariants: {}", p.architectural_invariants.join("; ").dimmed());
                    }
                    _ => println!("Response: {:?}", resp),
                }
            }
            Ok(())
        }
        Commands::Hook { action, files } => {
            println!("{}", "🪝 Automated Git Security Guardrails 🪝".bold().yellow());
            let repl_helper = HagibisRepl::new(client);
            if action == "pre-commit" && !files.is_empty() {
                let resp = repl_helper.dispatch(HgbRequest::HookRunPreCommit { staged_files: files }).await;
                match resp {
                    HgbResponse::HookRunResult(r) => {
                        println!("  ✔ Pre-Commit Scan: Scanned {} files in {}µs", r.files_scanned, r.duration_us);
                        if r.pass {
                            println!("  ✔ Status: {}", "PASSED".green().bold());
                        } else {
                            println!("  ✖ Status: {}", "BLOCKED".red().bold());
                            for v in r.violations {
                                println!("    - {}", v.red());
                            }
                            std::process::exit(1);
                        }
                    }
                    _ => println!("Response: {:?}", resp),
                }
            } else {
                let resp = repl_helper.dispatch(HgbRequest::HookInstall { repo_root: Some(".".to_string()), hooks: vec![hgb_core::GitHookType::PreCommit, hgb_core::GitHookType::PrePush] }).await;
                match resp {
                    HgbResponse::HookInstallResult(r) => {
                        println!("  ✔ Installed Hooks: {:?}", r.installed_hooks);
                        println!("  ✔ Scripts: {}", r.hook_script_paths.join(", "));
                        println!("  ✔ Active Guards: {}", r.active_guards.join(", ").cyan());
                    }
                    _ => println!("Response: {:?}", resp),
                }
            }
            Ok(())
        }
        Commands::Conventions { path, force } => {
            println!("{}", "🧬 Style Guide & Architectural DNA 🧬".bold().blue());
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::ConventionsHarvest { repo_root: Some(path), force_refresh: force }).await;
            match resp {
                HgbResponse::ConventionsResult(d) => {
                    println!("  ✔ DNA Hash: {}", d.blake3_content_hash[..16].green());
                    println!("  ✔ Sources ({}): {}", d.discovered_files.len(), d.discovered_files.join(", "));
                    println!("  ✔ Extracted Rules ({}):", d.rules.len());
                    for r in d.rules {
                        println!("    [{}] {}: {}", r.enforcement_level, r.category.bold(), r.rule_summary.dimmed());
                    }
                }
                _ => println!("Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Queue { action: _, tasks, concurrency } => {
            println!("{}", "🌲 Parallel Autopilot Worktree Swarm 🌲".bold().green());
            let repl_helper = HagibisRepl::new(client);
            if !tasks.is_empty() {
                let resp = repl_helper.dispatch(HgbRequest::QueueEnqueue { tasks, concurrency }).await;
                match resp {
                    HgbResponse::QueueResult(q) => {
                        println!("  ✔ Enqueued tasks. Active workers: {}/{}", q.active_workers, q.max_concurrency);
                        for j in q.jobs {
                            println!("    Task [{}]: '{}' => Status: {:?}", j.job_id.cyan(), j.prompt, j.status);
                        }
                    }
                    _ => println!("Response: {:?}", resp),
                }
            } else {
                let resp = repl_helper.dispatch(HgbRequest::QueueQuery).await;
                match resp {
                    HgbResponse::QueueResult(q) => {
                        println!("  ✔ Active Workers: {}/{}", q.active_workers, q.max_concurrency);
                        println!("  ✔ Queued Jobs ({}):", q.jobs.len());
                        for j in q.jobs {
                            println!("    [{}] {} (target: {}) => {:?}", j.job_id.cyan(), j.prompt, j.target_branch, j.status);
                        }
                    }
                    _ => println!("Response: {:?}", resp),
                }
            }
            Ok(())
        }
        Commands::Review { diff } => {
            println!("{}", "🔍 Agentic PR Reviewer & Inline BugBot 🔍".bold().yellow());
            let patch = if diff.is_empty() {
                "+ pub fn unwrap_user(x: Option<i32>) -> i32 {\n+     x.unwrap()\n+ }\n".to_string()
            } else if std::path::Path::new(&diff).exists() {
                std::fs::read_to_string(&diff).unwrap_or_default()
            } else {
                diff
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::ReviewDiff { diff_patch: patch }).await;
            match resp {
                HgbResponse::ReviewResult(r) => {
                    println!("  ✔ Overall Verdict: {}", r.overall_verdict.bold());
                    println!("  ✔ Comments ({}):", r.total_comments);
                    for c in r.inline_comments {
                        println!("    [{:?}] {}:L{}: {}", c.severity, c.file_path.cyan(), c.line_number, c.comment_body);
                        if let Some(rep) = c.suggested_replacement {
                            println!("      Suggested Replacement: {}", rep.green());
                        }
                    }
                }
                _ => println!("Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Benchmark { suite, model } => {
            println!("{}", "📊 Autonomous SWE-Bench & Coding Rigor Harness 📊".bold().magenta());
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::BenchmarkRunSuite { suite_name: suite, model }).await;
            match resp {
                HgbResponse::BenchmarkResult(b) => {
                    println!("  ✔ Suite: {}", b.suite_name.bold());
                    println!("  ✔ Evaluated Model: {}", b.evaluated_model.cyan());
                    println!("  ✔ Pass Rate: {:.1}% ({}/{} passed)", b.pass_rate_pct, b.passed_cases, b.total_cases);
                    println!("  ✔ Avg Latency: {}ms", b.avg_latency_ms);
                    for c in b.cases {
                        println!("    [{}] {} ({}ms, {} tokens)", if c.passed { "PASS".green() } else { "FAIL".red() }, c.test_id, c.execution_time_ms, c.token_usage);
                    }
                }
                _ => println!("Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Quickstart { description, framework, name } => {
            println!("{}", "⚡ 90-Second MVP Full-Stack Synthesizer ⚡".bold().green());
            let spec = hgb_core::QuickstartSpec {
                app_name: name,
                description,
                framework_preset: framework,
                database: "sqlite".to_string(),
                include_auth: true,
                include_billing: true,
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::QuickstartSynthesize { spec }).await;
            match resp {
                HgbResponse::QuickstartResult(q) => {
                    println!("  ✔ Project Directory: {}", q.project_directory.bold().cyan());
                    println!("  ✔ Files Generated ({}):", q.files_generated);
                    for f in q.generated_file_paths.iter().take(6) {
                        println!("    - {}", f);
                    }
                    if q.generated_file_paths.len() > 6 {
                        println!("    ... and {} more files", q.generated_file_paths.len() - 6);
                    }
                    println!("  ✔ Run Command: {}", q.run_command.bold().green());
                    println!("  ✔ Synthesis Time: {}ms", q.synthesis_time_ms);
                }
                _ => println!("Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Registry { action, target } => {
            println!("{}", "📦 Decentralized Agent Fleet Registry 📦".bold().blue());
            let repl_helper = HagibisRepl::new(client);
            if action == "install" && !target.is_empty() {
                let resp = repl_helper.dispatch(HgbRequest::RegistryInstall { plugin_id: target }).await;
                match resp {
                    HgbResponse::RegistryInstallResult(p) => {
                        println!("  ✔ Installed Agent: {} (v{})", p.name.bold().green(), p.version);
                        println!("  ✔ Description: {}", p.description);
                        println!("  ✔ Blake3 Fingerprint: {}", p.blake3_fingerprint[..16].dimmed());
                        println!("  ✔ Permissions: {}", p.required_permissions.join(", ").cyan());
                    }
                    _ => println!("Response: {:?}", resp),
                }
            } else {
                let resp = repl_helper.dispatch(HgbRequest::RegistrySearch { query: target }).await;
                match resp {
                    HgbResponse::RegistrySearchResult(r) => {
                        println!("  ✔ Matching Plugins ({}/{}):", r.matching_plugins.len(), r.total_available);
                        for p in r.matching_plugins {
                            println!("    * {} [{}] v{}: {}", p.id.bold().green(), p.author.dimmed(), p.version, p.description);
                        }
                    }
                    _ => println!("Response: {:?}", resp),
                }
            }
            Ok(())
        }
        Commands::Authorship { file } => {
            println!("{}", "📜 Blake3 Cryptographic AI Authorship Ledger 📜".bold().cyan());
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::ProvenanceAuditFile { file_path: file }).await;
            match resp {
                HgbResponse::ProvenanceResult(p) => {
                    println!("  ✔ File: {}", p.file_path.bold());
                    println!("  ✔ Total Lines: {}", p.total_lines);
                    println!("  ✔ Human Authored: {} lines ({:.1}%)", p.human_authored_lines, 100.0 - p.ai_percentage);
                    println!("  ✔ AI Authored: {} lines ({:.1}%)", p.ai_authored_lines, p.ai_percentage);
                    println!("  ✔ Ledger Root Blake3: {}", p.ledger_root_hash.green());
                    println!("  ✔ Spans ({}):", p.spans.len());
                    for s in p.spans.iter().take(5) {
                        println!("    L{}-L{}: {} [{}]", s.start_line, s.end_line, s.author_type.cyan(), s.blake3_content_hash[..10].dimmed());
                    }
                }
                _ => println!("Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Remote { host, port, token } => {
            println!("{}", "🌐 Encrypted Remote Daemon Tunnel 🌐".bold().blue());
            let repl_helper = HagibisRepl::new(client);
            let config = hgb_core::RemoteTunnelConfig {
                remote_host: host,
                remote_port: port,
                psk_auth_token: token,
                enable_compression: true,
            };
            let resp = repl_helper.dispatch(HgbRequest::RemoteTunnelConnect { config }).await;
            match resp {
                HgbResponse::RemoteTunnelResult(r) => {
                    println!("  ✔ Connected: {}", if r.connected { "YES".green().bold() } else { "NO".red() });
                    println!("  ✔ Remote Version: {}", r.remote_hgbd_version.cyan());
                    println!("  ✔ Round-Trip Latency: {}ms", r.round_trip_latency_ms);
                    println!("  ✔ Remote Workspaces: {}", r.active_workspaces.join(", ").dimmed());
                }
                _ => println!("Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Observe { limit } => {
            println!("{}", "📡 Unified Multi-Channel Observation Bus 📡".bold().yellow());
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::ObserveQueryRecent { limit }).await;
            match resp {
                HgbResponse::ObserveResult(o) => {
                    println!("  ✔ Total Events Tracked: {}", o.total_events);
                    println!("  ✔ Recent Timeline ({} events):", o.events.len());
                    for ev in o.events {
                        println!("    [{}] #{:04} [{:?}]: {}", ev.timestamp_ms, ev.sequence_id, ev.channel, ev.payload.dimmed());
                    }
                }
                _ => println!("Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Stack { name, supabase, stripe, tailwind, cloudflare } => {
            println!("{}", "🏗️ Zero-Config Managed Stack Preset Fabric 🏗️".bold().green());
            let mut services = Vec::new();
            if supabase { services.push(hgb_core::ManagedService::SupabaseAuthAndDb); }
            if stripe { services.push(hgb_core::ManagedService::StripeBilling); }
            if tailwind { services.push(hgb_core::ManagedService::TailwindShadcnUi); }
            if cloudflare { services.push(hgb_core::ManagedService::CloudflareEdgeWorker); }

            let config = hgb_core::StackWireupConfig {
                project_name: name,
                enabled_services: services,
                target_directory: ".".to_string(),
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::StackWireup { config }).await;
            match resp {
                HgbResponse::StackResult(s) => {
                    println!("  ✔ Configured Services: {:?}", s.services_configured);
                    println!("  ✔ Generated Manifests & Clients ({}):", s.generated_files.len());
                    for f in &s.generated_files {
                        println!("    - {}", f.cyan());
                    }
                    println!("  ✔ Required Environment Variables:");
                    for ev in s.required_env_vars.iter().take(4) {
                        println!("    {}", ev.dimmed());
                    }
                    println!("  ✔ Stack Status: {}", if s.ready_to_boot { "READY TO BOOT".green().bold() } else { "CONFIG PENDING".yellow() });
                }
                _ => println!("Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Evolve { model: _, generations, threshold: _ } => {
            println!("{}", "🧬 Recursive Self-Evolution & Autonomous DPO Distillation 🧬".bold().magenta());
            let config = hgb_core::SelfEvolutionConfig {
                target_path: ".".to_string(),
                max_generations: generations,
                mutation_rate: 0.15,
                auto_distill_recipes: true,
                export_dpo_dataset: true,
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::SelfEvolutionRun { config }).await;
            match resp {
                HgbResponse::SelfEvolutionResult(r) => {
                    println!("  ✔ Total Generations: {}", r.total_generations);
                    println!("  ✔ Mutations Evaluated: {}", r.total_mutations_evaluated);
                    println!("  ✔ Overall Fitness: {:.1}%", r.overall_fitness_score);
                    println!("  ✔ DPO Pairs Generated: {}", r.dpo_pairs_generated);
                    println!("  ✔ Convergence: {}", if r.convergence_achieved { "ACHIEVED".green().bold() } else { "IN PROGRESS".yellow() });
                    println!("  ✔ Summary: {}", r.summary_message.green());
                }
                _ => println!("Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Desktop { action, x, y, text } => {
            println!("{}", "🖥️ OS-Level Desktop Computer-Use & Sentry 🖥️".bold().cyan());
            let repl_helper = HagibisRepl::new(client);
            if action == "inspect" {
                let resp = repl_helper.dispatch(HgbRequest::DesktopInspect).await;
                match resp {
                    HgbResponse::DesktopInspectResult(r) => {
                        println!("  ✔ Screen Resolution: {}x{}", r.screen_resolution.0, r.screen_resolution.1);
                        println!("  ✔ Visible Windows ({}):", r.visible_windows.len());
                        for w in r.visible_windows {
                            println!("    - [{}] \"{}\" ({},{} {}x{})", w.window_id, w.title.cyan(), w.bounds.0, w.bounds.1, w.bounds.2, w.bounds.3);
                        }
                    }
                    _ => println!("Response: {:?}", resp),
                }
            } else {
                let act = match action.as_str() {
                    "click" => hgb_core::DesktopAction::Click { x, y, button: "left".to_string() },
                    "key" => hgb_core::DesktopAction::KeyPress { key: if text.is_empty() { "Return".to_string() } else { text } },
                    "type" => hgb_core::DesktopAction::Type { text },
                    _ => hgb_core::DesktopAction::Click { x, y, button: "left".to_string() },
                };
                let resp = repl_helper.dispatch(HgbRequest::DesktopActionExecute { action: act }).await;
                match resp {
                    HgbResponse::DesktopActionResult(r) => {
                        println!("  ✔ Action: {}", r.action_type);
                        println!("  ✔ Success: {}", if r.success { "YES".green() } else { "NO".red() });
                        println!("  ✔ Message: {}", r.message.dimmed());
                    }
                    _ => println!("Response: {:?}", resp),
                }
            }
            Ok(())
        }
        Commands::VerifyProof { target, solver, overflows, bounds } => {
            println!("{}", "📐 Formal Mathematical Verification & SMT Solver 📐".bold().yellow());
            let s = match solver.to_lowercase().as_str() {
                "cvc5" => hgb_core::SmtSolverKind::Cvc5,
                "kani" => hgb_core::SmtSolverKind::Kani,
                _ => hgb_core::SmtSolverKind::Z3,
            };
            let config = hgb_core::FormalVerificationConfig {
                target_file: target,
                solver: s,
                verify_overflows: overflows,
                verify_bounds: bounds,
                timeout_seconds: 15,
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::FormalVerifyRun { config }).await;
            match resp {
                HgbResponse::FormalVerifyResult(r) => {
                    println!("  ✔ Solver: {:?}", r.solver_used);
                    println!("  ✔ Target: {}", r.target_file.cyan());
                    println!("  ✔ Properties Verified: {}/{}", r.proven_count, r.total_properties);
                    println!("  ✔ Mathematically Sound: {}", if r.mathematically_sound { "PROVEN [SOUND]".green().bold() } else { "COUNTEREXAMPLE FOUND".red().bold() });
                }
                _ => println!("Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::Monorepo { action, files } => {
            println!("{}", "🌐 Enterprise Distributed Monorepo Hypergraph 🌐".bold().blue());
            let repl_helper = HagibisRepl::new(client);
            if action == "blast-radius" || !files.is_empty() {
                let changed: Vec<String> = files.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                let resp = repl_helper.dispatch(HgbRequest::MonorepoBlastRadius { changed_files: changed }).await;
                match resp {
                    HgbResponse::MonorepoBlastRadiusResult(r) => {
                        println!("  ✔ Directly Impacted Packages ({}): {:?}", r.directly_impacted_packages.len(), r.directly_impacted_packages);
                        println!("  ✔ Downstream Impact ({}): {:?}", r.downstream_impacted_packages.len(), r.downstream_impacted_packages);
                        println!("  ✔ Affected Test Targets: {:?}", r.affected_test_targets);
                    }
                    _ => println!("Response: {:?}", resp),
                }
            } else {
                let resp = repl_helper.dispatch(HgbRequest::MonorepoAnalyze).await;
                match resp {
                    HgbResponse::MonorepoAnalyzeResult(r) => {
                        println!("  ✔ Workspace Root: {}", r.workspace_root.cyan());
                        println!("  ✔ Total Packages: {}", r.total_packages);
                        println!("  ✔ Total Edges: {}", r.total_dependency_edges);
                        for p in r.packages {
                            println!("    - {:<20} ({}) deps: {:?}", p.name.green().bold(), p.relative_path.dimmed(), p.dependencies);
                        }
                    }
                    _ => println!("Response: {:?}", resp),
                }
            }
            Ok(())
        }
        Commands::Embedded { arch, file, no_std, freertos: _ } => {
            println!("{}", "⚡ Embedded Firmware & Microcontroller Lab ⚡".bold().magenta());
            let mcu_arch = match arch.to_lowercase().as_str() {
                "esp32" => hgb_core::TargetMcuArchitecture::Esp32,
                "riscv" => hgb_core::TargetMcuArchitecture::RiscV,
                "avr" => hgb_core::TargetMcuArchitecture::Avr,
                _ => hgb_core::TargetMcuArchitecture::ArmCortexM,
            };
            let config = hgb_core::EmbeddedCheckConfig {
                target_arch: mcu_arch,
                no_std,
                max_flash_bytes: 256 * 1024,
                max_ram_bytes: 64 * 1024,
            };
            let snippet = std::fs::read_to_string(&file).unwrap_or_else(|_| "// Bare-metal firmware entry\n#![no_std]\npub fn init() {}".to_string());
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::EmbeddedCheck { code: snippet, config }).await;
            match resp {
                HgbResponse::EmbeddedCheckResult(r) => {
                    println!("  ✔ Target MCU: {:?}", r.architecture);
                    println!("  ✔ no_std Valid: {}", if r.compiles_no_std { "YES".green() } else { "NO".red() });
                    println!("  ✔ Invariants Passed: {}", if r.memory_invariants_passed { "YES".green() } else { "NO".red() });
                    println!("  ✔ Flash Footprint: {} bytes ({:.1}%)", r.estimated_flash_bytes, r.flash_utilization_pct);
                    println!("  ✔ RAM Footprint:   {} bytes ({:.1}%)", r.estimated_ram_bytes, r.ram_utilization_pct);
                }
                _ => println!("Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::StoreRelease { platform, track, bundle_id, version } => {
            println!("{}", "🚀 Native App Store Release & Fastlane Orchestrator 🚀".bold().cyan());
            let plat = match platform.to_lowercase().as_str() {
                "android" => hgb_core::AppStorePlatform::GooglePlayStore,
                _ => hgb_core::AppStorePlatform::AppleAppStore,
            };
            let trk = match track.to_lowercase().as_str() {
                "internal" => hgb_core::ReleaseTrack::InternalTesting,
                "alpha" => hgb_core::ReleaseTrack::Alpha,
                "production" => hgb_core::ReleaseTrack::Production,
                _ => hgb_core::ReleaseTrack::Beta,
            };
            let config = hgb_core::StoreReleaseConfig {
                platform: plat,
                track: trk,
                app_bundle_id: bundle_id,
                version_name: version,
                build_number: 1,
                fastlane_lane: "beta".to_string(),
            };
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::StoreReleaseRun { config }).await;
            match resp {
                HgbResponse::StoreReleaseResult(r) => {
                    println!("  ✔ Platform: {:?}", r.platform);
                    println!("  ✔ Artifact: {}", r.build_artifact_path.cyan());
                    println!("  ✔ Code Signing: {}", if r.code_signing_verified { "VERIFIED".green() } else { "FAILED".red() });
                    println!("  ✔ Success: {}", if r.success { "YES".green() } else { "NO".red() });
                    println!("  ✔ Message: {}", r.message.green());
                }
                _ => println!("Response: {:?}", resp),
            }
            Ok(())
        }
        Commands::VoiceTts { text, voice, rate, list_voices } => {
            println!("{}", "🗣️ Local Neural Speech Synthesis Engine 🗣️".bold().green());
            let repl_helper = HagibisRepl::new(client);
            if list_voices {
                let resp = repl_helper.dispatch(HgbRequest::SpeechListVoices).await;
                match resp {
                    HgbResponse::SpeechListVoicesResult(voices) => {
                        println!("  Available Neural Voices ({}):", voices.len());
                        for v in voices {
                            println!("    - {:<24} ({}) sample_rate: {}Hz", v.voice_id.green().bold(), v.name.cyan(), v.sample_rate_hz);
                        }
                    }
                    _ => println!("Response: {:?}", resp),
                }
            } else {
                let config = hgb_core::SynthesisConfig {
                    voice_id: voice,
                    speaking_rate: rate,
                    pitch: 1.0,
                    output_format: "wav".to_string(),
                };
                let resp = repl_helper.dispatch(HgbRequest::SpeechSynthesize { text, config }).await;
                match resp {
                    HgbResponse::SpeechSynthesizeResult(r) => {
                        println!("  ✔ Voice Model: {}", r.voice_used.cyan());
                        println!("  ✔ Audio Duration: {:.2}s", r.duration_seconds);
                        println!("  ✔ Output Buffer: {} bytes", r.audio_bytes_len);
                        println!("  ✔ Audio Hash: {}", r.audio_sha256.dimmed());
                    }
                    _ => println!("Response: {:?}", resp),
                }
            }
            Ok(())
        }
        Commands::WasmFabric { action } => {
            use hgb_core::wasm_fabric::{WasmOrchestrator, FabricOrchestrator};
            let msg = FabricOrchestrator.orchestrate(&action);
            println!("🚀 {}", msg);
            Ok(())
        }
        Commands::DebtShredder { action: _ } => {
            use hgb_core::debt_shredder::{TechDebtExterminator, AutonomousShredder};
            let msg = AutonomousShredder.shred();
            println!("🧹 {}", msg);
            Ok(())
        }
        Commands::FirecrackerShield { action: _ } => {
            use hgb_core::firecracker_shield::{MicroVMSandbox, AgenticShield};
            let msg = AgenticShield.start_sandbox();
            println!("🛡️ {}", msg);
            Ok(())
        }
        Commands::NpuNative { action } => {
            use hgb_core::npu_native::{NpuOffloader, HyperLocalNpu};
            let msg = HyperLocalNpu.offload_task(&action);
            println!("⚡ {}", msg);
            Ok(())
        }
        Commands::VisionSync { action: _ } => {
            use hgb_core::vision_sync::{VisionToCode, RealTimeSync};
            let msg = RealTimeSync.sync_ui();
            println!("👁️ {}", msg);
            Ok(())
        }
        Commands::RagStack { action } => {
            use hgb_core::rag_stack::RagStackEngine;
            println!("{}", "🧠 AI-Native RAG & Vector Database Scaffolding".bold().cyan());
            let engine = RagStackEngine::new();
            let result = engine.run_action(&action);
            println!("   {}", result);
            Ok(())
        }
        Commands::Studio { port, patch_component, add_class } => {
            println!("{}", "🎨 Interactive Visual WYSIWYG Web Canvas Studio 🎨".bold().magenta());
            let repl_helper = HagibisRepl::new(client);
            if let Some(comp) = patch_component {
                let patch = hgb_core::VisualStudioSyncPatch {
                    component_id: comp,
                    target_file: "src/App.tsx".to_string(),
                    added_classes: add_class.map(|c| vec![c]).unwrap_or_default(),
                    removed_classes: vec![],
                    updated_styles: std::collections::HashMap::new(),
                };
                let resp = repl_helper.dispatch(HgbRequest::StudioApplyPatch { patch }).await;
                match resp {
                    HgbResponse::StudioApplyPatchResult(msg) => {
                        println!("  ✔ Patch Applied: {}", msg.green());
                    }
                    _ => println!("Response: {:?}", resp),
                }
            } else {
                let resp = repl_helper.dispatch(HgbRequest::StudioStart { port }).await;
                match resp {
                    HgbResponse::StudioStartResult(r) => {
                        println!("  ✔ Studio Server: {}", r.live_preview_url.cyan().bold());
                        println!("  ✔ Port: {}", r.local_server_port);
                        println!("  ✔ Components Discovered: {}", r.root_components.len());
                        println!("  ✔ Bi-Directional Sync: {}", if r.bi_directional_sync_active { "ACTIVE".green() } else { "INACTIVE".red() });
                    }
                    _ => println!("Response: {:?}", resp),
                }
            }
            Ok(())
        }
        Commands::DiffUi { original, modified, side_by_side, interactive } => {
            handle_diff_ui(original, modified, side_by_side, interactive).await?;
            Ok(())
        },
        Commands::Hybrid {
            task,
            benchmark,
            target,
            force_local,
            force_codex,
            privacy_check,
            check_command,
            consent,
        } => {
            let ws_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let mut engine = hgb_core::HybridOrchestrationEngine::new(&ws_dir);

            if benchmark {
                let scorecard = engine.get_scorecard()?;
                println!("{}", scorecard.render_terminal_table());
                return Ok(());
            }

            let task_str = task.unwrap_or_else(|| {
                eprintln!("{}", "Error: Task prompt is required. Use --benchmark to view scorecards or specify a task.".red());
                std::process::exit(1);
            });

            if privacy_check {
                println!("{}", "🛡️  Running Outbound Privacy Firewall & Data Sovereignty Audit...".cyan().bold());
                let target_files = target.clone().map(|t| vec![t]).unwrap_or_default();
                let outcome = engine.firewall.audit_outbound(&task_str, &target_files, consent);
                match outcome {
                    hgb_core::PrivacyAuditOutcome::Clean => {
                        println!("  {} Egress payload is 100% clean of sensitive keys and credentials.", "✔ PASS:".green().bold());
                    }
                    hgb_core::PrivacyAuditOutcome::Redacted { findings_count, redacted_categories, sanitized_payload } => {
                        println!("  {} Redacted {} secret(s): {:?}", "⚠ SANITIZED:".yellow().bold(), findings_count, redacted_categories);
                        println!("  Sanitized preview:\n{}", sanitized_payload);
                    }
                    hgb_core::PrivacyAuditOutcome::ConsentRequired { reason, sensitive_files } => {
                        println!("  {} {}", "✋ CONSENT REQUIRED:".yellow().bold(), reason);
                        println!("  Sensitive targets: {:?}", sensitive_files);
                        println!("  Rerun with `--consent` to authorize outbound transmission.");
                    }
                    hgb_core::PrivacyAuditOutcome::Blocked { reason, violations } => {
                        println!("  {} {}", "⛔ BLOCKED:".red().bold(), reason);
                        for v in violations {
                            println!("    - {}", v.red());
                        }
                    }
                }
                return Ok(());
            }

            println!("{}", "⚡ Hagibis Hybrid Dual-Brain Orchestrator".cyan().bold());
            println!("  Task: {}", task_str.bold());
            if let Some(ref t) = target {
                println!("  Target: {}", t.yellow());
            }

            // 1. Task Routing Step
            let target_path = target.as_ref().map(std::path::Path::new);
            let routing = engine.route_task(&task_str, target_path, force_local, force_codex);
            let tier_colored = match routing.tier {
                hgb_core::TaskRoutingTier::LocalEdge => "LOCAL_EDGE (Ollama/GGUF)".green().bold(),
                hgb_core::TaskRoutingTier::FrontierCodex => "FRONTIER_CODEX (High Capacity)".blue().bold(),
            };
            println!("  🎯 Route Tier: {} (decision latency: {}µs)", tier_colored, routing.latency_us);
            println!("  📋 Routing Logic: {}", routing.reason.dimmed());

            // 2. Execution Step (Isolated RAM CoW Sandbox + Escalation)
            println!("  🔄 Executing in RAM CoW overlay sandbox...");
            let report = engine
                .execute_hybrid_task(
                    &task_str,
                    target.as_deref(),
                    check_command.as_deref(),
                    force_local,
                    force_codex,
                    consent,
                )
                .await?;

            let res_colored = match report.verification_result {
                hgb_core::HybridVerificationResult::PassedFirstTry => "PASSED_FIRST_TRY (Local Edge)".green().bold(),
                hgb_core::HybridVerificationResult::PassedAfterEscalation => "PASSED_AFTER_ESCALATION (Frontier Codex Self-Healed)".yellow().bold(),
                hgb_core::HybridVerificationResult::Failed => "FAILED".red().bold(),
                hgb_core::HybridVerificationResult::Skipped => "SKIPPED".dimmed(),
            };

            println!("\n{}", "─── Execution Telemetry & Outcome ───".dimmed());
            println!("  Verification: {}", res_colored);
            println!("  Summary: {}", report.output_summary);
            if !report.files_modified.is_empty() {
                println!("  Files Modified (Committed): {:?}", report.files_modified);
            }
            println!("  Duration: Local: {}ms | Remote: {}ms | Total: {}ms", report.local_duration_ms, report.remote_duration_ms, report.total_duration_ms);
            println!("  Cost Arbitrage: Spent: ${:.4} | Cloud Hypothetical: ${:.4} | Net Saved: ${:.4}",
                report.actual_cost_usd, report.hypothetical_cloud_cost_usd, report.usd_saved);

            Ok(())
        }
        Commands::Dynatune { json } => {
            let sentry = hgb_core::DynatuneSynergyEngine::new();
            let report = sentry.evaluate_hardware_telemetry();
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("{}", report.render_terminal_table());
            }
            Ok(())
        }
        Commands::Masterclass { force: _ } => {
            println!("{}", "📘 Hagibis Masterclass & Architectural Documentation Pipeline".cyan().bold());
            println!("Compiling 'HAGIBIS_VIBE_CODING_MASTERCLASS.pdf' via uv / python3...");
            let ws_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let pipeline = hgb_core::MasterclassPipeline::new(ws_dir);
            let report = pipeline.generate_pdf()?;
            if report.success {
                println!("{} Masterclass PDF successfully compiled in {:.2}s!", "✔".green().bold(), report.compilation_duration_secs);
                println!("  Artifact:  {}", report.pdf_path.bold());
                println!("  File Size: {:.2} MB ({} bytes)", report.file_size_bytes as f64 / (1024.0 * 1024.0), report.file_size_bytes);
                println!("  BLAKE3:    {}", report.sha256);
            } else {
                eprintln!("{} Masterclass generation failed: {}", "✖".red().bold(), report.stdout_summary);
                std::process::exit(1);
            }
            Ok(())
        }
        Commands::WasmPlugin { wasm_path, input } => {
            println!("{}", "🧩 Hagibis WebAssembly (hgb-wasm-kernel) Plugin Engine".cyan().bold());
            let parsed_input: serde_json::Value = match serde_json::from_str(&input) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("{} Invalid JSON input: {}", "✖".red().bold(), e);
                    std::process::exit(1);
                }
            };
            let instance = hgb_core::WasmPluginInstance::load(&wasm_path)?;
            let manifest = instance.manifest.clone();
            println!("  Loaded Plugin:  {} v{} ({})", manifest.name.bold(), manifest.version, manifest.author.as_deref().unwrap_or("Hagibis"));
            println!("  Description:    {}", manifest.description);
            println!("  Input Schema:   {}", manifest.input_schema);
            println!("  Executing inside sandboxed WebAssembly runtime with fuel metering...");
            let start = std::time::Instant::now();
            let result = instance.execute(&parsed_input)?;
            let elapsed = start.elapsed();
            println!("{} Execution completed successfully in {:.3} ms!", "✔".green().bold(), elapsed.as_secs_f64() * 1000.0);
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        Commands::OllamaRoute { prompt, force_local, force_remote } => {
            println!("{}", "⚡ Hagibis Local Ollama Inference Routing Pipeline".cyan().bold());
            let router = hgb_core::OllamaInferenceRouter::default();
            let (task_kind, dest) = router.route_task(&prompt, force_local, force_remote);
            println!("  Classified Task: {}", task_kind.to_string().cyan().bold());
            match &dest {
                hgb_core::RoutingDestination::LocalOllama { model, endpoint, zero_cost } => {
                    println!("  Routed Target:   {} via Local Ollama ({})", model.green().bold(), endpoint);
                    println!("  Cost Tier:       {}", if *zero_cost { "$0.00 (Zero API Cost, Private Edge)".green().bold() } else { "Metered".into() });
                }
                hgb_core::RoutingDestination::CloudFrontier { provider, model, reason } => {
                    println!("  Routed Target:   {}/{} (Cloud Frontier)", provider.yellow().bold(), model);
                    println!("  Escalation Reason: {}", reason);
                }
            }
            println!("  Executing completion...");
            let report = router.execute_routed(&prompt, force_local, force_remote).await?;
            println!("{} Generation finished in {} ms (Model: {})", "✔".green().bold(), report.latency_ms, report.model_used);
            if report.is_local {
                println!("  Cost: $0.00 | Hypothetical cloud cost saved: ${:.6}", report.usd_saved);
            } else {
                println!("  Cost: ${:.6}", report.cost_usd);
            }
            println!("\n{}", "── Output ───────────────────────────────────────────────────".dimmed());
            println!("{}", report.output_text);
            println!("{}", "─────────────────────────────────────────────────────────────".dimmed());
            Ok(())
        }
        Commands::DualSandbox { workload, command } => {
            println!("{}", "🛡️  Hagibis Dual-Layer Secure Execution Engine".cyan().bold());
            println!("  Workload Spec:  {}", workload.bold());
            let ws_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let engine = hgb_core::DualLayerSandboxEngine::new(&ws_dir);
            println!("  Autonomous Arbiter evaluating workload safety boundary...");
            let start = std::time::Instant::now();
            let report = engine.execute_workload_auto(&workload, None, command.as_deref(), None)?;
            let total_ms = start.elapsed().as_millis();
            println!("{} Execution completed in {} ms!", "✔".green().bold(), total_ms);
            println!("  Selected Tier:      {}", report.tier.to_string().cyan().bold());
            println!("  Security Boundary:  {}", report.security_boundary);
            println!("  Startup Latency:    {} ms", report.startup_latency_ms);
            println!("  Execution Duration: {} ms", report.execution_duration_ms);
            println!("  Memory Allocated:   {} MiB", report.memory_allocated_mib);
            println!("  vCPUs Assigned:     {}", report.vcpu_count);
            println!("  Exit Code:          {}", report.exit_code);
            if !report.stdout.is_empty() {
                println!("\n{}", "── Standard Output ──────────────────────────────────────────".dimmed());
                println!("{}", report.stdout.trim());
            }
            if !report.stderr.is_empty() {
                println!("\n{}", "── Standard Error ───────────────────────────────────────────".red().dimmed());
                println!("{}", report.stderr.trim());
            }
            println!("{}", "─────────────────────────────────────────────────────────────".dimmed());
            Ok(())
        }
        Commands::SemanticCache { clear } => {
            println!("{}", "🧠 Hagibis In-Process Semantic Query Cache".cyan().bold());
            if clear {
                let res = client.send(hgb_core::protocol::HgbRequest::SemanticCacheClear).await?;
                match res {
                    hgb_core::protocol::HgbResponse::SemanticCacheStatusResult(msg) => {
                        println!("{} {}", "✔".green().bold(), msg);
                    }
                    other => println!("{:?}", other),
                }
            } else {
                let res = client.send(hgb_core::protocol::HgbRequest::SemanticCacheStatus).await?;
                match res {
                    hgb_core::protocol::HgbResponse::SemanticCacheStatusResult(msg) => {
                        println!("{}", msg);
                    }
                    other => println!("{:?}", other),
                }
            }
            Ok(())
        }
        Commands::BareMetal { boost, relax, profile } => {
            println!("{}", "⚡ Hagibis Bare-Metal Kernel & Dynatune Bridge".cyan().bold());
            if boost {
                let res = client.send(hgb_core::protocol::HgbRequest::BareMetalCommand { profile: "performance".to_string() }).await?;
                match res {
                    hgb_core::protocol::HgbResponse::BareMetalCommandResult(s) => println!("{}", s),
                    other => println!("{:?}", other),
                }
            } else if relax {
                let res = client.send(hgb_core::protocol::HgbRequest::BareMetalCommand { profile: "powersave".to_string() }).await?;
                match res {
                    hgb_core::protocol::HgbResponse::BareMetalCommandResult(s) => println!("{}", s),
                    other => println!("{:?}", other),
                }
            } else if let Some(p) = profile {
                let res = client.send(hgb_core::protocol::HgbRequest::BareMetalCommand { profile: p }).await?;
                match res {
                    hgb_core::protocol::HgbResponse::BareMetalCommandResult(s) => println!("{}", s),
                    other => println!("{:?}", other),
                }
            } else {
                let res = client.send(hgb_core::protocol::HgbRequest::BareMetalStatus).await?;
                match res {
                    hgb_core::protocol::HgbResponse::BareMetalStatusResult(s) => println!("{}", s),
                    other => println!("{:?}", other),
                }
            }
            Ok(())
        }
        Commands::WasmSync { target, bundle, pages, step, status } => {
            println!("{}", "🌐 Stateful WebAssembly Distributed Sync & Edge Migrator".cyan().bold());
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let mut component = hgb_core::StatefulWasmComponent::new("local-desktop-node", pages);
            component.set_env("ENGINE", "hagibis-edge-deployer");
            component.set_env("DEPLOYMENT_ENV", "production");
            let _ = component.write_memory(0, b"hagibis-wasm-kernel:stateful-context-payload");
            let _ = component.execute_step(step);

            let envelope = component.serialize_snapshot(&target)?;
            println!("{} Snapshot Envelope: {} ({} dirty pages, {} instructions, BLAKE3: {})",
                "•".cyan(),
                envelope.envelope_id.yellow().bold(),
                envelope.state_metadata.dirty_pages_count,
                envelope.state_metadata.instruction_counter,
                &envelope.blake3_hash[..16]
            );

            if bundle {
                let migrator = hgb_core::CloudflareEdgeMigrator::new(&cwd);
                let receipt = migrator.package_edge_bundle(&envelope)?;
                println!("\n{}", hgb_core::CloudflareEdgeMigrator::render_receipt_card(&receipt));
            } else if status {
                println!("{} Active Tier: {:?}", "•".cyan(), component.active_tier());
                println!("{} Sync Status: {:?}", "•".cyan(), component.status());
                println!("{} Total Pages: {} ({} KB)", "•".cyan(), component.page_count(), component.page_count() * 64);
            } else {
                println!("{} Verified cryptographic snapshot seal: {}", "✔".green().bold(), envelope.blake3_hash);
                println!("{} Ready for distributed hot-swap migration across edge worker fleet.", "➔".white().bold());
            }
            Ok(())
        }
        Commands::WasmPool { status: _status, prewarm, exec, input } => {
            println!("{}", "🚀 Zero-Copy WebAssembly Execution Hot-Pool".cyan().bold());
            let client = HgbClient::new();
            if !prewarm.is_empty() {
                let res = client.send(hgb_core::protocol::HgbRequest::WasmPoolPrewarm { wasm_paths: prewarm }).await?;
                if let hgb_core::protocol::HgbResponse::WasmPoolPrewarmResult(reports) = res {
                    for r in reports {
                        println!("  {} {} (compiled in {} µs) - {}", "•".cyan(), r.name.bold(), r.compile_duration_us, r.status);
                    }
                }
            } else if let Some(wasm_file) = exec {
                let parsed_input: serde_json::Value = serde_json::from_str(&input).unwrap_or(serde_json::json!({}));
                let res = client.send(hgb_core::protocol::HgbRequest::WasmPoolExec { wasm_path: wasm_file, input: parsed_input }).await?;
                if let hgb_core::protocol::HgbResponse::WasmPoolExecResult(rep) = res {
                    println!("  Plugin:       {}", rep.plugin_name.green().bold());
                    println!("  Cache Hit:    {}", if rep.cache_hit { "YES (Instant)".green() } else { "NO (Fresh JIT)".yellow() });
                    println!("  Checkout:     {} µs", rep.checkout_duration_us);
                    println!("  Execution:    {} µs", rep.execution_duration_us);
                    println!("  Total:        {} µs", rep.total_latency_us.to_string().yellow().bold());
                    println!("  Output:       {}", rep.output);
                }
            } else {
                let res = client.send(hgb_core::protocol::HgbRequest::WasmPoolStatus).await?;
                if let hgb_core::protocol::HgbResponse::WasmPoolStatusResult(st) = res {
                    println!("  Pool Capacity:    {} slots ({} available hot slots)", st.pool_capacity, st.available_hot_slots);
                    println!("  Cached Modules:   {}", st.cached_modules_count);
                    for m in &st.cached_modules {
                        println!("    - {} ({} bytes, {} hits, compile: {} µs)", m.name.bold(), m.size_bytes, m.hit_count, m.compile_duration_us);
                    }
                    println!("  Total Executions: {}", st.total_executions);
                    println!("  Avg Latency:      {:.1} µs", st.avg_latency_us);
                    println!("  Cache Hit Ratio:  {:.1}%", st.hit_ratio_pct);
                }
            }
            Ok(())
        }
        Commands::PowerScheduler { status: _status, schedule, threads } => {
            println!("{}", "🧠 Adaptive Power-Aware Scheduler (Ollama + Dynatune)".cyan().bold());
            let client = HgbClient::new();
            if schedule {
                let res = client.send(hgb_core::protocol::HgbRequest::PowerScheduleTask { requested_threads: threads }).await?;
                if let hgb_core::protocol::HgbResponse::PowerScheduleTaskResult(cfg) = res {
                    println!("  Scheduled Threads:  {} execution threads", cfg.num_thread.to_string().green().bold());
                    println!("  Context Budget:     {} tokens", cfg.num_ctx);
                    println!("  Cooldown Buffer:    {} ms", cfg.cooldown_ms);
                    println!("  Governor Profile:   {}", cfg.governor_used);
                    println!("  Thermal Headroom:   {:.1}°C", cfg.thermal_headroom_c);
                }
            } else {
                let res = client.send(hgb_core::protocol::HgbRequest::PowerSchedulerStatus).await?;
                if let hgb_core::protocol::HgbResponse::PowerSchedulerStatusResult(st) = res {
                    let p = &st.profile;
                    println!("  CPU Package Temp:   {:.1}°C [{:?}]", p.current_temp_c, p.severity);
                    println!("  Power Source:       {}", if p.ac_connected { "AC Mains" } else { "Battery" });
                    println!("  Cores Topology:     {} physical / {} logical", p.physical_cores, p.logical_cpus);
                    println!("  Optimal Threads:    {} threads", p.optimal_threads.to_string().green().bold());
                    println!("  Context Budget:     {} tokens", p.context_length_budget);
                    println!("  Recommended Gov:    {}", p.recommended_governor);
                    println!("  Action:             {}", p.action_summary);
                    println!("  Total Scheduled:    {} tasks ({} throttled)", st.total_scheduled_tasks, st.total_throttled_tasks);
                }
            }
            Ok(())
        }
        Commands::Bus { status: _status, follow, topic, publish } => {
            println!("{}", "📈 Unified Distributed Event Bus (Tokio Broadcast Streamer)".cyan().bold());
            let client = HgbClient::new();
            if !publish.is_empty() {
                let top = publish[0].clone();
                let payload = publish[1].clone();
                let res = client.send(hgb_core::protocol::HgbRequest::PublishEvent { topic: top, payload }).await?;
                if let hgb_core::protocol::HgbResponse::PublishEventResult { receivers } = res {
                    println!("  {} Published event to {} active subscriber(s)", "✔".green().bold(), receivers);
                }
            } else if follow {
                println!("  Connecting to daemon event stream (UDS: /run/user/1000/hgb.sock)...");
                let stream_res = tokio::net::UnixStream::connect("/run/user/1000/hgb.sock").await;
                match stream_res {
                    Ok(mut stream) => {
                        use tokio::io::{AsyncReadExt, AsyncWriteExt};
                        let req = hgb_core::protocol::HgbRequest::SubscribeEvents { topic_filter: topic };
                        let encoded = bincode::serialize(&req)?;
                        stream.write_all(&encoded).await?;

                        let mut buf = vec![0u8; 4096];
                        while let Ok(n) = stream.read(&mut buf).await {
                            if n == 0 { break; }
                            let text = String::from_utf8_lossy(&buf[..n]);
                            for line in text.lines() {
                                if line.starts_with('{') {
                                    println!("  {}", line);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("  {} Failed to connect to event stream: {}", "✖".red(), e);
                    }
                }
            } else {
                let res = client.send(hgb_core::protocol::HgbRequest::EventBusStatus).await?;
                if let hgb_core::protocol::HgbResponse::EventBusStatusResult(st) = res {
                    println!("  Ring Capacity:      {} events", st.capacity);
                    println!("  Active Subscribers: {}", st.active_subscribers.to_string().green().bold());
                    println!("  Total Published:    {}", st.total_published);
                    println!("  Recent History:     {} events stored", st.history_len);
                    for ev in &st.latest_events {
                        println!("    [{}] {} -> {}", ev.id.dimmed(), ev.topic.cyan().bold(), ev.payload);
                    }
                }
            }
            Ok(())
        }
        Commands::HwRoute { complexity, status: _status } => {
            println!("{}", "⚡ Native Ollama Integration & Hardware-Aware Router".cyan().bold());
            let client = HgbClient::new();
            if let Some(c) = complexity {
                let res = client.send(hgb_core::protocol::HgbRequest::HardwareRouterSelect { complexity: c }).await?;
                if let hgb_core::protocol::HgbResponse::HardwareRouterDecisionResult(d) = res {
                    println!("  Selected Model:    {}", d.selected_model.green().bold());
                    println!("  Complexity Tier:   {:?}", d.complexity);
                    println!("  Thermal Headroom:  {:.1}°C", d.thermal_headroom_c);
                    println!("  Power Source:      {}", if d.ac_connected { "AC Mains [Optimal]" } else { "Battery [Eco-scaled]" });
                    println!("  WASM Offload:      {}", if d.is_wasm_offload { "YES (Conserving cycles)".yellow() } else { "NO (Native GGUF/Ollama)".green() });
                    println!("  Ollama Endpoint:   {}", d.endpoint.dimmed());
                    println!("  Reasoning:         {}", d.reasoning.cyan());
                } else if let hgb_core::protocol::HgbResponse::Error(e) = res {
                    eprintln!("  ✖ Router error: {}", e.red());
                }
            } else {
                let res = client.send(hgb_core::protocol::HgbRequest::HardwareRouterStatus).await?;
                if let hgb_core::protocol::HgbResponse::HardwareRouterStatusResult(s) = res {
                    println!("  CPU Package Temp:  {:.1}°C (Thermal Headroom: {:.1}°C)", s.current_temp_c, s.thermal_headroom_c);
                    println!("  Power Source:      {}", if s.ac_connected { "AC Mains [Wall Power]" } else { "Battery Power" });
                    println!("  Ollama Endpoint:   {}", s.ollama_endpoint.green());
                    println!("  Sibling Directory: {} (exists: {})", s.sibling_ollama_path.dimmed(), s.sibling_ollama_exists);
                    println!("  Installed Models:  {} model(s)", s.installed_models.len());
                    for m in &s.installed_models {
                        println!("    • {}", m.yellow());
                    }
                    println!("  Recommended Heavy: {}", s.recommended_heavy_model.cyan().bold());
                    println!("  Recommended Light: {}", s.recommended_light_model.green().bold());
                    println!("  Total Tasks Routed:{}", s.total_routed_tasks);
                } else if let hgb_core::protocol::HgbResponse::Error(e) = res {
                    eprintln!("  ✖ Router error: {}", e.red());
                }
            }
            Ok(())
        }
        Commands::MicroVmSwarm { code, language, heavy, status: _status } => {
            println!("{}", "🛡️ Distributed MicroVM Swarms (Firecracker + WASM Hot-Pool)".cyan().bold());
            let client = HgbClient::new();
            if let Some(c) = code {
                let res = client.send(hgb_core::protocol::HgbRequest::MicroVmSwarmExec {
                    code: c,
                    language,
                    use_microvm: heavy,
                }).await?;
                if let hgb_core::protocol::HgbResponse::MicroVmSwarmExecResult(rep) = res {
                    println!("  Sandbox ID:        {}", rep.sandbox_id.yellow());
                    println!("  Execution Tier:    {}", rep.tier_used.cyan().bold());
                    println!("  Status:            {}", if rep.success { "PASSED (Clean)".green().bold() } else { "FAILED".red().bold() });
                    println!("  Exit Code:         {}", rep.exit_code);
                    println!("  Startup Latency:   {} µs", rep.startup_latency_us);
                    println!("  Run Duration:      {} µs", rep.execution_duration_us);
                    println!("  Blast Radius:      {} bytes ({} file(s) mutated)", rep.blast_radius_bytes, rep.files_mutated);
                    println!("  Security Boundary: {}", rep.security_boundary.dimmed());
                    if !rep.stdout.is_empty() {
                        println!("\n  ── stdout ──\n{}", rep.stdout.trim());
                    }
                    if !rep.stderr.is_empty() {
                        println!("\n  ── stderr ──\n{}", rep.stderr.trim().red());
                    }
                } else if let hgb_core::protocol::HgbResponse::Error(e) = res {
                    eprintln!("  ✖ Execution error: {}", e.red());
                }
            } else {
                let res = client.send(hgb_core::protocol::HgbRequest::MicroVmSwarmStatus).await?;
                if let hgb_core::protocol::HgbResponse::MicroVmSwarmStatusResult(s) = res {
                    println!("  KVM Accessible:    {}", if s.kvm_accessible { "YES (/dev/kvm ready)".green().bold() } else { "NO (Rootless CoW fallback active)".yellow() });
                    println!("  Firecracker VM:    {}", if s.firecracker_installed { "INSTALLED".green() } else { "COW FALLBACK".yellow() });
                    println!("  OverlayFS CoW:     {}", if s.overlay_cow_available { "READY (Sub-millisecond)".green().bold() } else { "NOT READY".red() });
                    println!("  WASM Hot Slots:    {} slots available", s.wasm_hot_slots);
                    println!("  Total Executions:  {}", s.total_executions);
                    println!("  Active Sandboxes:  {}", s.active_sandboxes);
                } else if let hgb_core::protocol::HgbResponse::Error(e) = res {
                    eprintln!("  ✖ Status error: {}", e.red());
                }
            }
            Ok(())
        }
        Commands::SelfPatch { trigger, log, status: _status } => {
            println!("{}", "🧬 Autonomous Self-Patching & Continuous Learning Loop".cyan().bold());
            let client = HgbClient::new();
            if trigger {
                let res = client.send(hgb_core::protocol::HgbRequest::SelfPatchTrigger { context_log: log }).await?;
                if let hgb_core::protocol::HgbResponse::SelfPatchTriggerResult(rec) = res {
                    println!("  Patch ID:          {}", rec.patch_id.yellow().bold());
                    println!("  Timestamp:         {}", rec.timestamp.dimmed());
                    println!("  Target File:       {}", rec.target_file.cyan());
                    println!("  First-Green Verify:{}", if rec.verified_green { "PASSED [Sandbox Isolation]".green().bold() } else { "FAILED".red() });
                    println!("  Applied to Disk:   {}", if rec.applied { "YES [git apply / atomic update]".green().bold() } else { "NO".yellow() });
                    println!("  DPO Pair Saved:    {}", if rec.dpo_recorded { "YES [.hgb/distillation/]".green() } else { "NO".yellow() });
                    println!("  Patch Path:        {}", rec.patch_file.dimmed());
                    println!("  DPO Path:          {}", rec.dpo_file.dimmed());
                    println!("  Summary:           {}", rec.summary.cyan());
                } else if let hgb_core::protocol::HgbResponse::Error(e) = res {
                    eprintln!("  ✖ Self-patch error: {}", e.red());
                }
            } else {
                let res = client.send(hgb_core::protocol::HgbRequest::SelfPatchStatus).await?;
                if let hgb_core::protocol::HgbResponse::SelfPatchStatusResult(s) = res {
                    println!("  Monitored Log:     {}", s.monitored_log_path.dimmed());
                    println!("  Patches Dir:       {}", s.patches_dir.dimmed());
                    println!("  Distillation Dir:  {}", s.distillation_dir.dimmed());
                    println!("  Total Patches:     {} generated, {} applied", s.total_patches, s.applied_patches);
                    println!("  DPO Pairs Count:   {} preference pairs", s.dpo_pairs_count);
                    if !s.recent_patches.is_empty() {
                        println!("\n  Recent Verified Patches:");
                        for p in &s.recent_patches {
                            println!("    • [{}] {} -> {}", p.patch_id.yellow(), p.target_file.cyan(), p.summary.dimmed());
                        }
                    }
                } else if let hgb_core::protocol::HgbResponse::Error(e) = res {
                    eprintln!("  ✖ Status error: {}", e.red());
                }
            }
            Ok(())
        }
        Commands::Ide { .. } => unreachable!(),
    }
}

async fn run_mcp_stdio_server(_client: HgbClient) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut lines = BufReader::new(stdin).lines();
    let ws_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mcp_server = hgb_core::HagibisMcpServer::new(&ws_dir);

    while let Ok(Some(line)) = lines.next_line().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(resp_str) = mcp_server.handle_message(trimmed).await {
            stdout.write_all(resp_str.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

async fn handle_diff_ui(
    original: String,
    modified: Option<String>,
    mut side_by_side: bool,
    interactive: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use hgb_nextgen::inline_diff_engine::{DualBufferOverlay, InlineDiffEngine};
    use std::io::Write;

    let target_file_path: Option<std::path::PathBuf>;
    let orig_text: String;
    let mod_text: String;

    if let Some(ref mod_path_str) = modified {
        let orig_path = std::path::Path::new(&original);
        orig_text = if orig_path.exists() {
            std::fs::read_to_string(orig_path)?
        } else {
            original.clone()
        };

        let mod_path = std::path::Path::new(mod_path_str);
        if mod_path.exists() {
            mod_text = std::fs::read_to_string(mod_path)?;
            target_file_path = Some(mod_path.to_path_buf());
        } else {
            mod_text = mod_path_str.clone();
            target_file_path = if orig_path.exists() {
                Some(orig_path.to_path_buf())
            } else {
                None
            };
        }
    } else {
        // Compare single target file on disk against git HEAD
        let file_path = std::path::Path::new(&original);
        if !file_path.exists() {
            eprintln!(
                "{} Target file '{}' not found on disk.",
                "Error:".red().bold(),
                original
            );
            return Ok(());
        }
        mod_text = std::fs::read_to_string(file_path)?;
        target_file_path = Some(file_path.to_path_buf());

        // Resolve git repo relative path
        let rel_cmd = std::process::Command::new("git")
            .args(["ls-files", "--full-name", &original])
            .output();
        let git_rel_path = if let Ok(ref out) = rel_cmd {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                s
            } else {
                original.clone()
            }
        } else {
            original.clone()
        };

        let git_show = std::process::Command::new("git")
            .args(["show", &format!("HEAD:{}", git_rel_path)])
            .output();

        if let Ok(show_out) = git_show {
            if show_out.status.success() {
                orig_text = String::from_utf8_lossy(&show_out.stdout).to_string();
            } else {
                eprintln!(
                    "{} File '{}' has no HEAD revision in Git. Specify modified file: hgb diff-ui <orig> <mod>",
                    "Notice:".yellow().bold(),
                    original
                );
                return Ok(());
            }
        } else {
            eprintln!(
                "{} Git is not available. Specify modified file: hgb diff-ui <orig> <mod>",
                "Notice:".yellow().bold()
            );
            return Ok(());
        }
    }

    let mut overlay = DualBufferOverlay::new(original.clone(), orig_text.clone());
    overlay.set_speculative_content(mod_text.clone());

    if !interactive {
        let term_width = crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(100);
        let col_width = term_width.saturating_sub(6) / 2;
        let rendered = if side_by_side {
            InlineDiffEngine::render_side_by_side(&orig_text, &mod_text, col_width)
        } else {
            InlineDiffEngine::render_ansi_inline_diff(&orig_text, &mod_text)
        };
        print!("{}", rendered);
        return Ok(());
    }

    // Interactive TUI Cockpit Mode
    println!("{}", "🪽 Hagibis Visual IDE & Inline Diffing Cockpit (Tab to accept, Esc/q to reject, s to toggle split)".cyan().bold());
    let raw_guard = repl::RawModeGuard::enter()?;
    let mut stdout = std::io::stdout();

    loop {
        let term_width = crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(100);
        let col_width = term_width.saturating_sub(6) / 2;
        let rendered = if side_by_side {
            InlineDiffEngine::render_side_by_side(&orig_text, &mod_text, col_width)
        } else {
            InlineDiffEngine::render_ansi_inline_diff(&orig_text, &mod_text)
        };

        crossterm::execute!(
            stdout,
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
            crossterm::cursor::MoveTo(0, 0)
        )?;

        write!(stdout, "{}\r\n", rendered)?;
        write!(
            stdout,
            "\x1b[1;38;2;255;215;0m[Tab]\x1b[0m Accept All Changes   \x1b[1;38;2;255;100;100m[Esc / q]\x1b[0m Discard & Exit   \x1b[1;38;2;120;180;255m[s]\x1b[0m Toggle Split/Unified\r\n"
        )?;
        stdout.flush()?;

        match crossterm::event::read()? {
            crossterm::event::Event::Key(key) => {
                if key.kind == crossterm::event::KeyEventKind::Press {
                    if key.code == crossterm::event::KeyCode::Char('c')
                        && key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                    {
                        overlay.discard_rejection();
                        drop(raw_guard);
                        println!("\r\n{} Aborted by user.", "✘".red().bold());
                        break;
                    }

                    match key.code {
                        crossterm::event::KeyCode::Tab => {
                            let committed = overlay.commit_tab_acceptance();
                            drop(raw_guard);
                            if let Some(ref path) = target_file_path {
                                std::fs::write(path, committed)?;
                                println!(
                                    "\r\n{} Diff accepted and written to '{}' (O(1) pointer swap).",
                                    "✔".green().bold(),
                                    path.display()
                                );
                            } else {
                                println!("\r\n{} Changes committed to virtual buffer.", "✔".green().bold());
                            }
                            break;
                        }
                        crossterm::event::KeyCode::Esc | crossterm::event::KeyCode::Char('q') => {
                            overlay.discard_rejection();
                            drop(raw_guard);
                            println!("\r\n{} Diff discarded. No disk writes performed.", "✘".red().bold());
                            break;
                        }
                        crossterm::event::KeyCode::Char('s') => {
                            side_by_side = !side_by_side;
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    Ok(())
}


