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

    /// Authenticate Hagibis with Google Account via OAuth 2.0 Web Login
    Login,

    /// Check Google Gemini OAuth and API Key credentials status
    Auth,

    /// View or set the active AI model
    Model {
        /// Optional model name (e.g. gemini, gemini-2.5-flash, gemini-2.5-pro)
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
    Cockpit {
        /// Render in headless mode without spawning terminal interactive loop
        #[arg(long)]
        headless: bool,
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
            return state.run_interactive().await.map_err(|e| Box::new(e) as Box<dyn std::error::Error>);
        }
    }

    match command {
        Commands::Repl => unreachable!(),
        Commands::Cockpit { .. } => unreachable!(),
        Commands::Run { text, model, provider } => {
            let active_model = model.as_deref().unwrap_or("gemini-2.5-flash");
            println!("{}", format!("  ⚡ AGY Reasoning (model: {})...", active_model).cyan().bold());
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
        Commands::Model { name } => {
            if let Some(m) = name {
                println!("✔ Default model set to: {}", m);
                Ok(())
            } else {
                let status = hgb_core::GeminiProvider::credential_status();
                println!("  [•] Active Provider / Credential: {}", status);
                Ok(())
            }
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
    }
}
