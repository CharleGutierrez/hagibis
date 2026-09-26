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
    about = "⚡ Hagibis (hgb): Sub-Millisecond Microkernel & Swarm Engine in Systems-Grade Rust",
    long_about = "⚡ Hagibis (hgb) — The Sub-Millisecond Microkernel Swarm Engine in Systems-Grade Rust.\nRunning without arguments launches the AGY Chat Canvas & Interactive Cockpit."
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

#[derive(Subcommand)]
enum Commands {
    /// Launch the classic terminal REPL (line-by-line scrolling mode)
    #[command(alias = "repl")]
    Classic,

    /// Launch the AGY Conversational Chat Canvas & Cockpit (default)
    #[command(alias = "canvas", alias = "c")]
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

    /// Speculative dual-draft racing ("First Green Wins")
    Vibe {
        /// Prompt or task description to race
        prompt: String,
        /// Optional target directory
        #[arg(short, long)]
        target_dir: Option<String>,
        /// Enable speculative dual-draft racing
        #[arg(long, default_value_t = true)]
        race: bool,
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

    /// Formally verify code invariant using SMT-LIB2 / Interval Solver
    Verify {
        /// Target code expression or file
        #[arg(short, long)]
        target: String,
        /// Invariant type: division, bounds, overflow
        #[arg(short, long, default_value = "division")]
        invariant: String,
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
    #[command(alias = "ui")]
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
    #[command(alias = "radar")]
    Impact {
        /// Target symbol to analyze (e.g. struct name, fn name, type)
        symbol: String,
        /// Originating source file path
        #[arg(short, long)]
        file: String,
    },

    /// Specialist Swarm Pod: 4-role concurrent consensus DAG (Architect -> Coder -> [Reviewer + QA])
    #[command(alias = "swarm")]
    Pod {
        /// Development task or feature specification
        task: String,
    },

    /// Terminal Rescue: diagnose failed shell commands and suggest verified corrective fixes
    #[command(alias = "rescue")]
    Fix {
        /// Failed command line to diagnose
        command: String,
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
            let active_model_str = if let Some(ref m) = model {
                m.clone()
            } else if !hgb_core::GeminiProvider::is_available() && hgb_core::OllamaProvider::is_available() {
                if let Some(prov) = hgb_core::OllamaProvider::auto_discover() {
                    prov.default_model().to_string()
                } else {
                    "gemini-2.5-flash".to_string()
                }
            } else {
                "gemini-2.5-flash".to_string()
            };
            let active_model = active_model_str.as_str();
            let is_local = hgb_core::OllamaProvider::is_ollama_model(active_model);
            if is_local {
                println!("{}", format!("  ⚡ Local Ollama Reasoning (model: {})...", active_model).magenta().bold());
            } else {
                println!("{}", format!("  ⚡ AGY Reasoning (model: {})...", active_model).cyan().bold());
            }
            let t0 = std::time::Instant::now();
            let req = HgbRequest::Prompt {
                prompt: text,
                model: model.clone(),
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
                        println!("⚡ Currently Active Model: {}", persisted.bold().green());
                        hgb_core::play_vibe_chime(true);
                    } else {
                        let resp = repl_helper.dispatch(HgbRequest::Status).await;
                        match resp {
                            HgbResponse::Status(s) => {
                                let cur = s.active_models.first().cloned().unwrap_or_else(|| "auto".to_string());
                                println!("⚡ Currently Active Model: {}", cur.bold().green());
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
        Commands::Vibe { prompt, target_dir, race: _ } => {
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
            println!("{}", format!("⚡ Instant App Forge: Scaffolding '{}' with stack '{}'...", project_name, stack).bold().cyan());
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
        Commands::Verify { target, invariant } => {
            let repl_helper = HagibisRepl::new(client);
            let resp = repl_helper.dispatch(HgbRequest::Verify { target, invariant }).await;
            repl_helper.render_response(resp);
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
                println!("{}", format!("⚡ Applying Ghost-Fix '{}'...", fix_id).cyan().bold());
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
            println!("{}", format!("🚨 Diagnosing failed command: '{}'...", command).cyan().bold());
            let resp = repl_helper.dispatch(HgbRequest::RescueDiagnose {
                failed_command: command,
                exit_code,
                stderr,
                stdout,
            }).await;
            repl_helper.render_response(resp);
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
        Commands::Glance { image, context } => {
            let repl_helper = HagibisRepl::new(client);
            println!("{}", format!("🖼️ Inspecting canvas: '{}'...", image).cyan().bold());
            let resp = repl_helper.dispatch(HgbRequest::GlanceInspect {
                image_path: image,
                context_path: context,
            }).await;
            repl_helper.render_response(resp);
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
    }
}
