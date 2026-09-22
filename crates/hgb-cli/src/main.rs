mod client;
mod repl;

use clap::{Parser, Subcommand};
use client::HgbClient;
use hgb_core::HgbRequest;
use repl::HagibisRepl;

#[derive(Parser)]
#[command(
    name = "hgb",
    bin_name = "hgb",
    version,
    about = "⚡ Hagibis (hgb): Sub-Millisecond Microkernel & Swarm Engine in Systems-Grade Rust",
    long_about = "⚡ Hagibis (hgb) — The Sub-Millisecond Microkernel Swarm Engine in Systems-Grade Rust.\nRunning without arguments launches the interactive REPL."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Launch the interactive terminal REPL (default when run without arguments)
    #[command(alias = "chat", alias = "i")]
    Repl,

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
        /// Case-insensitive search
        #[arg(short, long)]
        ignore_case: bool,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let client = HgbClient::new();

    // Default to Interactive REPL if no subcommand provided
    let command = match cli.command {
        Some(cmd) => cmd,
        None => Commands::Repl,
    };

    if let Commands::Repl = command {
        let mut repl = HagibisRepl::new(client);
        return repl.run().await;
    }

    let req = match command {
        Commands::Repl => unreachable!(),
        Commands::Run { text, model, provider } => HgbRequest::Prompt {
            prompt: text,
            model,
            provider,
            stream: false,
        },
        Commands::Ping => HgbRequest::Ping,
        Commands::Status => HgbRequest::Status,
        Commands::Doctor => HgbRequest::Doctor,
        Commands::Verify { target, invariant } => HgbRequest::Verify { target, invariant },
        Commands::Checkpoint { action, label } => HgbRequest::Checkpoint { action, label },
        Commands::Provenance { action } => HgbRequest::Provenance { action },
        Commands::Fuzz { target, iterations } => HgbRequest::Fuzz { target, iterations },
        Commands::Mesh => HgbRequest::MeshStatus,
        Commands::View { path, start, end, offset } => HgbRequest::CrudView {
            path,
            start_line: start,
            end_line: end,
            offset,
        },
        Commands::Write { path, content, overwrite } => HgbRequest::CrudWrite {
            path,
            content,
            overwrite,
        },
        Commands::Edit { path, target, replacement, start, end, multiple } => HgbRequest::CrudEdit {
            path,
            target,
            replacement,
            start_line: start,
            end_line: end,
            allow_multiple: multiple,
        },
        Commands::Ls { path } => HgbRequest::CrudList { path },
        Commands::Grep { pattern, path, ignore_case } => HgbRequest::CrudGrep {
            pattern,
            path: Some(path),
            case_insensitive: ignore_case,
        },
    };

    let repl_helper = HagibisRepl::new(client);
    let resp = repl_helper.dispatch(req).await;
    repl_helper.render_response(resp);

    Ok(())
}
