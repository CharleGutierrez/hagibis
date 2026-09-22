use crate::client::HgbClient;
use colored::Colorize;
use hgb_core::{HgbRequest, HgbResponse};
use std::io::{self, BufRead, Write};

pub struct HagibisRepl {
    client: HgbClient,
    model: Option<String>,
}

impl HagibisRepl {
    pub fn new(client: HgbClient) -> Self {
        Self {
            client,
            model: None,
        }
    }

    pub async fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        println!("{}", "================================================================================".cyan());
        println!("{}", " ⚡ HAGIBIS (hgb) INTERACTIVE REPL v0.1.0 ⚡ ".bold().cyan());
        println!("{}", " Sub-Millisecond Microkernel & Swarm Engine in Systems-Grade Rust".italic());
        println!("{}", " Type any prompt to execute, or '/help' for slash commands. '/exit' to quit.".dimmed());
        println!("{}", "================================================================================".cyan());

        let stdin = io::stdin();
        let mut reader = stdin.lock();

        loop {
            print!("{} ", "hgb ❯".bold().green());
            io::stdout().flush()?;

            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 {
                break; // EOF
            }

            let input = line.trim();
            if input.is_empty() {
                continue;
            }

            if input.starts_with('/') {
                if !self.handle_slash_command(input).await? {
                    break;
                }
            } else {
                self.execute_prompt(input).await?;
            }
        }

        println!("{}", "👋 Goodbye from Hagibis!".cyan());
        Ok(())
    }

    async fn handle_slash_command(&mut self, input: &str) -> Result<bool, Box<dyn std::error::Error>> {
        let parts: Vec<&str> = input.split_whitespace().collect();
        let cmd = parts[0].to_lowercase();
        let args = if parts.len() > 1 { parts[1..].join(" ") } else { String::new() };

        match cmd.as_str() {
            "/exit" | "/quit" | "/q" => return Ok(false),
            "/help" | "/?" | "/h" => self.print_help(),
            "/clear" | "/cls" => {
                print!("\x1B[2J\x1B[1;1H");
                io::stdout().flush()?;
            }
            "/ping" => {
                let resp = self.dispatch(HgbRequest::Ping).await;
                self.render_response(resp);
            }
            "/status" => {
                let resp = self.dispatch(HgbRequest::Status).await;
                self.render_response(resp);
            }
            "/doctor" | "/doc" => {
                let resp = self.dispatch(HgbRequest::Doctor).await;
                self.render_response(resp);
            }
            "/model" => {
                if args.is_empty() {
                    let current = self.model.as_deref().unwrap_or("auto");
                    println!("  [•] Active Model: {}", current.yellow());
                } else {
                    self.model = Some(args.clone());
                    println!("  ✔ Model set to: {}", args.green());
                }
            }
            "/provenance" | "/prov" => {
                let action = if args.is_empty() { "append".to_string() } else { args };
                let resp = self.dispatch(HgbRequest::Provenance { action }).await;
                self.render_response(resp);
            }
            "/checkpoint" | "/ckpt" => {
                let label = if args.is_empty() { None } else { Some(args) };
                let resp = self.dispatch(HgbRequest::Checkpoint { action: "create".to_string(), label }).await;
                self.render_response(resp);
            }
            "/fuzz" => {
                let target = if args.is_empty() { "sample_target".to_string() } else { args };
                let resp = self.dispatch(HgbRequest::Fuzz { target, iterations: 100 }).await;
                self.render_response(resp);
            }
            "/verify" => {
                let target = if args.is_empty() { "x > 0".to_string() } else { args };
                let resp = self.dispatch(HgbRequest::Verify { target, invariant: "division".to_string() }).await;
                self.render_response(resp);
            }
            "/mesh" => {
                let resp = self.dispatch(HgbRequest::MeshStatus).await;
                self.render_response(resp);
            }
            _ => {
                println!("{} Unknown slash command '{}'. Type '/help' for available commands.", "⚠".yellow(), cmd);
            }
        }

        Ok(true)
    }

    async fn execute_prompt(&self, prompt: &str) -> Result<(), Box<dyn std::error::Error>> {
        let req = HgbRequest::Prompt {
            prompt: prompt.to_string(),
            model: self.model.clone(),
            provider: None,
            stream: false,
        };
        let resp = self.dispatch(req).await;
        self.render_response(resp);
        Ok(())
    }

    pub async fn dispatch(&self, req: HgbRequest) -> HgbResponse {
        match self.client.send(req.clone()).await {
            Ok(resp) => resp,
            Err(_e) => {
                // Standalone fallback
                let state = std::sync::Arc::new(hgb_daemon::server::DaemonState::new(self.client.socket_path().to_path_buf()));
                hgb_daemon::server::HagibisDaemon::handle_request(&state, req).await
            }
        }
    }

    pub fn render_response(&self, resp: HgbResponse) {
        match resp {
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
        }
    }

    fn print_help(&self) {
        println!("{}", "⚡ Hagibis Interactive REPL Commands ⚡".bold().cyan());
        println!("  {}", "--- Core Commands ---".dimmed());
        println!("  {:<20} {}", "/help, /?".green(), "Show this help table");
        println!("  {:<20} {}", "/clear, /cls".green(), "Clear terminal screen");
        println!("  {:<20} {}", "/exit, /quit".green(), "Exit interactive REPL");
        println!("  {:<20} {}", "/ping".green(), "Measure UDS IPC latency (in microseconds)");
        println!("  {:<20} {}", "/status".green(), "Display daemon status and memory RSS");
        println!("  {:<20} {}", "/model [name]".green(), "View or set active model");
        println!();
        println!("  {}", "--- Next-Era Engines ---".dimmed());
        println!("  {:<20} {}", "/doctor, /doc".cyan(), "Run full health audit across all pillars");
        println!("  {:<20} {}", "/provenance [act]".cyan(), "Append or audit Blake3 Merkle ledger");
        println!("  {:<20} {}", "/checkpoint [lbl]".cyan(), "Create or view time-travel state snapshot");
        println!("  {:<20} {}", "/fuzz <target>".cyan(), "Run property-based differential fuzzer");
        println!("  {:<20} {}", "/verify <target>".cyan(), "Formally verify invariants with SMT-LIB2");
        println!("  {:<20} {}", "/mesh".cyan(), "Display P2P swarm mesh status");
        println!();
        println!("  {}", "Pro-tip: Any plain text without a '/' prefix executes as an AI swarm prompt.".italic().dimmed());
    }
}
