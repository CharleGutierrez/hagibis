mod client;

use clap::{Parser, Subcommand};
use client::HgbClient;
use colored::Colorize;
use hgb_core::{HgbRequest, HgbResponse};

#[derive(Parser)]
#[command(name = "hgb", bin_name = "hgb", version, about = "⚡ Hagibis (hgb): Sub-Millisecond Microkernel & Swarm Engine in Systems-Grade Rust")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Ping the resident hgbd daemon and measure IPC latency
    Ping,
    /// Inspect daemon status, uptime, active models, and memory RSS
    Status,
    /// Run diagnostic check across all microkernel pillars
    Doctor,
    /// Submit prompt for instant execution
    Prompt {
        /// User prompt text
        text: String,
        /// Model identifier
        #[arg(short, long)]
        model: Option<String>,
        /// Provider identifier
        #[arg(short, long)]
        provider: Option<String>,
    },
    /// Formally verify code invariant using SMT-LIB2 / Interval Solver
    Verify {
        /// Target code expression
        #[arg(short, long)]
        target: String,
        /// Invariant description (division, bounds, overflow)
        #[arg(short, long, default_value = "division")]
        invariant: String,
    },
    /// Manage time-travel state checkpoints
    Checkpoint {
        /// Action: create, list, rollback
        #[arg(short, long, default_value = "create")]
        action: String,
        /// Checkpoint label
        #[arg(short, long)]
        label: Option<String>,
    },
    /// Audit and inspect Blake3 Merkle Provenance Ledger
    Provenance {
        /// Action: append, root
        #[arg(short, long, default_value = "append")]
        action: String,
    },
    /// Run differential fuzzing mutation suite against target
    Fuzz {
        /// Target input string
        #[arg(short, long)]
        target: String,
        /// Iteration count
        #[arg(short, long, default_value = "100")]
        iterations: usize,
    },
    /// Check ZeroConf P2P Swarm Mesh status
    Mesh,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let client = HgbClient::new();

    let req = match cli.command {
        Commands::Ping => HgbRequest::Ping,
        Commands::Status => HgbRequest::Status,
        Commands::Doctor => HgbRequest::Doctor,
        Commands::Prompt { text, model, provider } => HgbRequest::Prompt {
            prompt: text,
            model,
            provider,
            stream: false,
        },
        Commands::Verify { target, invariant } => HgbRequest::Verify { target, invariant },
        Commands::Checkpoint { action, label } => HgbRequest::Checkpoint { action, label },
        Commands::Provenance { action } => HgbRequest::Provenance { action },
        Commands::Fuzz { target, iterations } => HgbRequest::Fuzz { target, iterations },
        Commands::Mesh => HgbRequest::MeshStatus,
    };

    match client.send(req.clone()).await {
        Ok(resp) => match resp {
            HgbResponse::Pong { latency_us } => {
                println!("{} Daemon pong received in {} µs", "✔ PONG:".green().bold(), latency_us);
            }
            HgbResponse::Status(status) => {
                println!("{}", "⚡ HAGIBIS RESIDENT DAEMON STATUS ⚡".bold().cyan());
                println!("  [•] Version: {}", status.version.yellow());
                println!("  [•] Uptime: {} secs", status.uptime_secs);
                println!("  [•] Memory RSS: {:.1} MB", status.memory_rss_mb);
                println!("  [•] Active Models: {:?}", status.active_models);
                println!("  [•] Connected Peers: {}", status.active_peers);
                println!("  [•] Socket: {}", status.socket_path.cyan());
            }
            HgbResponse::DoctorReport(pillars) => {
                println!("{}", "================================================================================".cyan());
                println!("{}", " 🏛️ HAGIBIS MICROKERNEL SYSTEMS REPORT 🏛️ ".bold().cyan());
                println!("{}", "================================================================================".cyan());
                for p in pillars {
                    println!("  ✔ {} [{}]: {}", p.name.bold(), p.status.green(), p.message);
                }
                println!("{}", "================================================================================".cyan());
            }
            HgbResponse::Complete { output, tokens_used, duration_ms } => {
                println!("{}", output);
                if tokens_used > 0 {
                    println!("  {} {} tokens in {} ms", "⏱️".cyan(), tokens_used, duration_ms);
                }
            }
            HgbResponse::TextChunk(chunk) => print!("{}", chunk),
            HgbResponse::Error(err) => eprintln!("{} {}", "✖ ERROR:".red().bold(), err),
        },
        Err(e) => {
            // Standalone fallback if daemon is not running
            eprintln!("{} {}. Executing in standalone microkernel mode...", "ℹ NOTICE:".yellow().bold(), e);
            let state = std::sync::Arc::new(hgb_daemon::server::DaemonState::new(client.socket_path().to_path_buf()));
            let resp = hgb_daemon::server::HagibisDaemon::handle_request(&state, req).await;
            match resp {
                HgbResponse::Complete { output, .. } => println!("{}", output),
                HgbResponse::DoctorReport(pillars) => {
                    println!("{}", "================================================================================".cyan());
                    println!("{}", " 🏛️ HAGIBIS MICROKERNEL STANDALONE REPORT 🏛️ ".bold().cyan());
                    println!("{}", "================================================================================".cyan());
                    for p in pillars {
                        println!("  ✔ {} [{}]: {}", p.name.bold(), p.status.green(), p.message);
                    }
                    println!("{}", "================================================================================".cyan());
                }
                _ => println!("{:?}", resp),
            }
        }
    }

    Ok(())
}
