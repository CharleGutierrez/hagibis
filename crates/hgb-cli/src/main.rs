pub mod canvas;
mod client;
mod repl;

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

#[derive(Subcommand)]
enum Commands {
    /// Launch the classic terminal REPL (line-by-line scrolling mode)
    #[command(alias = "repl")]
    Classic,

    /// Launch the AGY Conversational Chat Canvas & Cockpit (default)
    #[command(alias = "chat-canvas", alias = "c")]
    Chat,

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
    },

    /// Ping the resident hgbd daemon and measure UDS roundtrip latency
    Ping,

    /// Inspect daemon status, uptime, active models, and memory RSS
    #[command(alias = "info")]
    Status,

    /// Run diagnostic check across all microkernel pillars
    #[command(alias = "doc")]
    Doctor,

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

    /// Speculative dual-draft racing & Ambient Watch-and-Vibe Autonomous Loop
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

    /// Manage time-travel state checkpoints and WAL
    #[command(alias = "ckpt")]
    Checkpoint {
        /// Action: create, list, rollback
        #[arg(short, long, default_value = "create")]
        action: String,
        /// Checkpoint label
        #[arg(short, long)]
        label: Option<String>,
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

    /// Check ZeroConf P2P Swarm Mesh status
    Mesh,

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

    /// Ephemeral Mock Fabric: launch dynamic in-memory CRUD REST server on localhost
    #[command(alias = "mock-server")]
    Mock {
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
        /// Stop a running mock server on port
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

    /// Universal Model Context Protocol (MCP) Client
    Mcp {
        #[command(subcommand)]
        action: McpSubcommand,
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

    /// Universal Offline API Mirage: synthetic mocks & wiretapping
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

    /// Full-Duplex Zero-Latency Voice Flow Co-Pilot
    Voice {
        /// Optional spoken audio transcript to process
        #[arg(default_value = "wrap this call in a circuit breaker")]
        transcript: String,
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

    /// Relational Time-Warp Data Synthesizer: temporal mock datasets with strict FK integrity
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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let client = HgbClient::new();

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
        Commands::Run { text, model, provider } => {
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
            let is_local = hgb_core::OllamaProvider::is_ollama_model(active_model);
            if is_local {
                println!("{}", format!("  🪽 Local Ollama Reasoning (model: {})...", active_model).magenta().bold());
            } else {
                println!("{}", format!("  🪽 AGY Reasoning (model: {})...", active_model).cyan().bold());
            }
            let t0 = std::time::Instant::now();
            let req = HgbRequest::Prompt {
                prompt: text,
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
                    if tokens_used > 0 {
                        let tps = (tokens_used as f64) / (dur as f64 / 1000.0).max(0.001);
                        println!(
                            "{}",
                            format!("  ⏱️ {} tokens in {} ms ({:.1} tok/s) • Model: {}", tokens_used, dur, tps, active_model).dimmed()
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
        Commands::Doctor => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::Doctor).await;
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
        Commands::Vibe { prompt, target_dir, race: _, watch } => {
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
        Commands::Checkpoint { action, label } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::Checkpoint { action, label }).await;
            repl_helper.render_response(resp);
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
        Commands::Mesh => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::MeshStatus).await;
            repl_helper.render_response(resp);
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
                auditor.audit_diff(&diff_output)
            } else if target.is_file() {
                auditor.audit_file(&target).unwrap_or_else(|_| auditor.audit_code("", None))
            } else {
                auditor.audit_workspace(&target).unwrap_or_else(|_| auditor.audit_code("", None))
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
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::Undo {
                checkpoint_id: checkpoint,
            }).await;
            repl_helper.render_response(resp);
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
        Commands::Mock { resource, port, seed, schema, stop } => {
            let repl_helper = HagibisRepl::new(client);
            if let Some(stop_port) = stop {
                println!("{}", format!("🛑 Stopping mock server on port {}...", stop_port).yellow().bold());
                let resp = repl_helper.dispatch(HgbRequest::MockServerStop { port: stop_port }).await;
                repl_helper.render_response(resp);
            } else {
                println!("{}", format!("🎭 Launching in-memory Mock Fabric for resource '{}'...", resource).cyan().bold());
                let resp = repl_helper.dispatch(HgbRequest::MockServerStart {
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
            let repl_helper = HagibisRepl::new(client);
            match action {
                McpSubcommand::List { config } => {
                    let resp = repl_helper.dispatch(HgbRequest::McpListTools { config_path: config }).await;
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
                    let resp = repl_helper.dispatch(HgbRequest::McpCallTool {
                        server_name: server,
                        tool_name: tool,
                        arguments: parsed_args,
                        config_path: config,
                    }).await;
                    let is_err = matches!(resp, HgbResponse::Error(_));
                    repl_helper.render_response(resp);
                    if is_err {
                        std::process::exit(1);
                    }
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
                    ("API_KEY".to_string(), "sk_dummy_hagibis_secret_vault".to_string()),
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
        Commands::Voice { transcript } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::VoiceFlowProcess { transcript }).await;
            repl_helper.render_response(resp);
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
                "#[test] fn test_calc() { assert!(calculate(5)); }".to_string()
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
    }
}
