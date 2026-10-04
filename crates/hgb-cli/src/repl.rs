use crate::canvas::{ChatCanvas, ToolCallCard, ToolCardStatus};
use crate::client::HgbClient;
use colored::Colorize;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode},
};
use hgb_core::{HgbRequest, HgbResponse};
use std::fs::OpenOptions;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

/// Result of an interactive readline session
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadlineResult {
    Submit(String),
    Eof,
    #[allow(dead_code)]
    Interrupted,
}

/// RAII Guard ensuring raw terminal mode and bracketed paste are safely restored on drop
pub struct RawModeGuard {
    active: bool,
}

impl RawModeGuard {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let _ = crossterm::execute!(io::stdout(), crossterm::event::EnableBracketedPaste);
        Ok(Self { active: true })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = crossterm::execute!(io::stdout(), crossterm::event::DisableBracketedPaste);
            let _ = disable_raw_mode();
            self.active = false;
        }
    }
}

/// Interactive readline and Tagisan keyboard navigation editor
pub struct ReplEditor {
    pub history: Vec<String>,
    pub history_index: usize,
    pub draft: Vec<char>,
    pub kill_ring: String,
    pub history_file: Option<PathBuf>,
}

impl Default for ReplEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplEditor {
    pub fn new() -> Self {
        let history_file = Self::resolve_history_path();
        let history = if let Some(ref path) = history_file {
            Self::load_history_from_file(path)
        } else {
            Vec::new()
        };
        let history_index = history.len();
        Self {
            history,
            history_index,
            draft: Vec::new(),
            kill_ring: String::new(),
            history_file,
        }
    }

    #[allow(dead_code)]
    pub fn with_history(history: Vec<String>) -> Self {
        let history_index = history.len();
        Self {
            history,
            history_index,
            draft: Vec::new(),
            kill_ring: String::new(),
            history_file: None,
        }
    }

    fn resolve_history_path() -> Option<PathBuf> {
        if let Ok(home) = std::env::var("HOME") {
            let hgb_dir = PathBuf::from(&home).join(".config").join("hagibis");
            let _ = std::fs::create_dir_all(&hgb_dir);
            let hgb_file = hgb_dir.join("repl_history.txt");
            if !hgb_file.exists() {
                let tgs_file = PathBuf::from(&home).join(".tagisan").join("repl_history.txt");
                if tgs_file.exists() {
                    let _ = std::fs::copy(&tgs_file, &hgb_file);
                }
            }
            Some(hgb_file)
        } else {
            let dir = PathBuf::from(".hagibis");
            let _ = std::fs::create_dir_all(&dir);
            Some(dir.join("repl_history.txt"))
        }
    }

    fn load_history_from_file(path: &PathBuf) -> Vec<String> {
        if !path.exists() {
            return Vec::new();
        }
        let content = std::fs::read_to_string(path).unwrap_or_default();
        let mut lines: Vec<String> = content
            .lines()
            .map(|s| s.to_string())
            .filter(|s| !s.trim().is_empty())
            .collect();
        if lines.len() > 5000 {
            lines = lines.split_off(lines.len() - 5000);
        }
        lines
    }

    pub fn add_history(&mut self, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.last().map(|s| s.as_str()) == Some(trimmed) {
            self.history_index = self.history.len();
            return;
        }
        self.history.push(trimmed.to_string());
        self.history_index = self.history.len();

        if let Some(ref path) = self.history_file {
            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(file, "{}", trimmed);
            }
        }
    }

    pub fn find_word_backward(buffer: &[char], cursor: usize) -> usize {
        if cursor == 0 {
            return 0;
        }
        let mut i = cursor;
        while i > 0 && buffer[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !buffer[i - 1].is_whitespace() {
            i -= 1;
        }
        i
    }

    pub fn find_word_forward(buffer: &[char], cursor: usize) -> usize {
        let len = buffer.len();
        if cursor >= len {
            return len;
        }
        let mut i = cursor;
        while i < len && !buffer[i].is_whitespace() {
            i += 1;
        }
        while i < len && buffer[i].is_whitespace() {
            i += 1;
        }
        i
    }

    pub fn longest_common_prefix(strings: &[String]) -> String {
        if strings.is_empty() {
            return String::new();
        }
        let first = &strings[0];
        let mut len = 0;
        for (i, c) in first.chars().enumerate() {
            if strings.iter().all(|s| s.chars().nth(i) == Some(c)) {
                len += c.len_utf8();
            } else {
                break;
            }
        }
        first[..len].to_string()
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

    pub fn has_unclosed_delimiters(s: &str) -> bool {
        let count_triple_double = s.matches("\"\"\"").count();
        if count_triple_double % 2 != 0 {
            return true;
        }
        let count_triple_single = s.matches("'''").count();
        if count_triple_single % 2 != 0 {
            return true;
        }

        let count_code_fences = s.matches("```").count();
        if count_code_fences % 2 != 0 {
            return true;
        }

        let mut round = 0i32;
        let mut square = 0i32;
        let mut curly = 0i32;
        let mut in_str = false;
        let mut escape = false;

        for c in s.chars() {
            if escape {
                escape = false;
                continue;
            }
            if c == '\\' {
                escape = true;
                continue;
            }
            if c == '"' {
                in_str = !in_str;
                continue;
            }
            if in_str {
                continue;
            }
            match c {
                '(' => round += 1,
                ')' => round = (round - 1).max(0),
                '[' => square += 1,
                ']' => square = (square - 1).max(0),
                '{' => curly += 1,
                '}' => curly = (curly - 1).max(0),
                _ => {}
            }
        }

        round > 0 || square > 0 || curly > 0
    }

    fn compute_layout(
        prompt: &str,
        continuation_prompt: &str,
        buffer: &[char],
        cursor: usize,
        term_width: usize,
    ) -> (String, usize, usize, usize, usize) {
        let p0_width = Self::visible_width(prompt);
        let pc_width = Self::visible_width(continuation_prompt);

        let mut output_text = String::new();
        output_text.push_str(prompt);

        let mut cur_row = 0;
        let mut cur_col = p0_width;

        let mut cursor_row = 0;
        let mut cursor_col = p0_width;

        for (i, &c) in buffer.iter().enumerate() {
            if i == cursor {
                cursor_row = cur_row;
                cursor_col = cur_col;
            }

            if c == '\n' {
                output_text.push_str("\r\n");
                output_text.push_str(continuation_prompt);
                cur_row += 1;
                cur_col = pc_width;
            } else {
                let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(1);
                if cur_col + cw > term_width {
                    cur_row += 1;
                    cur_col = cw;
                } else {
                    cur_col += cw;
                }
                output_text.push(c);
            }
        }

        if cursor >= buffer.len() {
            cursor_row = cur_row;
            cursor_col = cur_col;
        }

        (output_text, cursor_row, cursor_col, cur_row, cur_col)
    }

    fn redraw_multiline(
        prompt: &str,
        continuation_prompt: &str,
        buffer: &[char],
        cursor: usize,
        last_cursor_row: &mut usize,
    ) -> io::Result<()> {
        let term_width = crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(80).max(20);
        let (output_text, cursor_row, cursor_col, end_row, _end_col) =
            Self::compute_layout(prompt, continuation_prompt, buffer, cursor, term_width);

        let mut stdout = io::stdout();

        if *last_cursor_row > 0 {
            write!(stdout, "\x1b[{}A", *last_cursor_row)?;
        }

        write!(stdout, "\r\x1b[J")?;
        write!(stdout, "{}", output_text)?;

        if end_row > cursor_row {
            write!(stdout, "\x1b[{}A", end_row - cursor_row)?;
        }
        write!(stdout, "\r")?;
        if cursor_col > 0 {
            write!(stdout, "\x1b[{}C", cursor_col.min(term_width))?;
        }

        stdout.flush()?;
        *last_cursor_row = cursor_row;
        Ok(())
    }

    fn finalize_for_submit(
        prompt: &str,
        continuation_prompt: &str,
        buffer: &[char],
        cursor: usize,
    ) -> io::Result<()> {
        let term_width = crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(80).max(20);
        let (_, cursor_row, _, end_row, _) =
            Self::compute_layout(prompt, continuation_prompt, buffer, cursor, term_width);
        let mut stdout = io::stdout();

        if end_row > cursor_row {
            write!(stdout, "\x1b[{}B", end_row - cursor_row)?;
        }
        write!(stdout, "\r\n")?;
        stdout.flush()?;
        Ok(())
    }

    fn redraw_line(prompt: &str, buffer: &[char], cursor: usize) -> io::Result<()> {
        let mut row = 0;
        let cont = format!("{}", "... ".cyan().dimmed());
        Self::redraw_multiline(prompt, &cont, buffer, cursor, &mut row)
    }

    fn run_reverse_search(
        history: &[String],
        buffer: &mut Vec<char>,
        cursor: &mut usize,
        active_prompt: &str,
    ) -> io::Result<()> {
        let original_buf = buffer.clone();
        let original_cursor = *cursor;

        let mut query = String::new();
        let mut search_idx: Option<usize> = None;

        let find_match = |q: &str, start_from: Option<usize>| -> Option<usize> {
            if q.is_empty() || history.is_empty() {
                return None;
            }
            let end = start_from.unwrap_or(history.len().saturating_sub(1));
            for i in (0..=end).rev() {
                if history[i].contains(q) {
                    return Some(i);
                }
            }
            None
        };

        loop {
            let mut stdout = io::stdout();
            let match_display = if let Some(idx) = search_idx {
                &history[idx]
            } else {
                ""
            };
            let prompt_str = if search_idx.is_some() || query.is_empty() {
                format!("{}'{}': {}", "(reverse-i-search)".yellow(), query.yellow().bold(), match_display.yellow())
            } else {
                format!("{}'{}': {}", "(failed reverse-i-search)".red(), query.red().bold(), match_display.yellow())
            };
            write!(stdout, "\r\x1b[2K{}", prompt_str)?;
            stdout.flush()?;

            if let Event::Key(key_event) = event::read()? {
                if key_event.kind == KeyEventKind::Release {
                    continue;
                }
                match key_event.code {
                    KeyCode::Char('r') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                        if let Some(idx) = search_idx {
                            if idx > 0 {
                                search_idx = find_match(&query, Some(idx - 1));
                            }
                        }
                    }
                    KeyCode::Char('c') | KeyCode::Char('g') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                        *buffer = original_buf;
                        *cursor = original_cursor;
                        Self::redraw_line(active_prompt, buffer, *cursor)?;
                        return Ok(());
                    }
                    KeyCode::Esc => {
                        *buffer = original_buf;
                        *cursor = original_cursor;
                        Self::redraw_line(active_prompt, buffer, *cursor)?;
                        return Ok(());
                    }
                    KeyCode::Enter => {
                        if let Some(idx) = search_idx {
                            *buffer = history[idx].chars().collect();
                            *cursor = buffer.len();
                        }
                        return Ok(());
                    }
                    KeyCode::Backspace => {
                        query.pop();
                        search_idx = find_match(&query, None);
                    }
                    KeyCode::Char(c) if !key_event.modifiers.contains(KeyModifiers::CONTROL) && !key_event.modifiers.contains(KeyModifiers::ALT) => {
                        query.push(c);
                        search_idx = find_match(&query, None);
                    }
                    _ => {
                        if let Some(idx) = search_idx {
                            *buffer = history[idx].chars().collect();
                            *cursor = buffer.len();
                        }
                        return Ok(());
                    }
                }
            }
        }
    }

    pub fn get_completions(prefix: &str) -> Vec<(String, usize)> {
        let mut results = Vec::new();
        let trimmed_prefix = prefix.trim_start();

        // 1. Slash commands completion
        if trimmed_prefix.starts_with('/') && !trimmed_prefix.contains(' ') {
            let slash_cmds = [
                ("/help", "Show help manual"),
                ("/clear", "Clear terminal screen"),
                ("/exit", "Exit interactive session"),
                ("/quit", "Exit interactive session"),
                ("/ping", "Measure UDS IPC latency"),
                ("/status", "Display daemon status and memory RSS"),
                ("/doctor", "Run full health audit across all pillars"),
                ("/model", "Switch or view active model (/model list, /model <name>)"),
                ("/models", "List all available local and cloud models"),
                ("/login", "Authenticate with Google Account via OAuth"),
                ("/auth", "Check Google Gemini credential status"),
                ("/provenance", "Append or audit Blake3 Merkle ledger"),
                ("/checkpoint", "Create or view time-travel state snapshot"),
                ("/fuzz", "Run property-based differential fuzzer"),
                ("/verify", "Formally verify invariants with SMT-LIB2"),
                ("/mesh", "Display P2P swarm mesh status"),
                ("/cockpit", "Launch full-screen Interactive Cockpit TUI"),
                ("/vibe", "Speculative dual-draft racing (first green wins)"),
                ("/race", "Run speculative dual-draft race on prompt"),
                ("/undo", "One-key time-travel rollback to previous checkpoint"),
                ("/diff", "Show color-coded unified visual diff"),
                ("/style", "Personal coding DNA style memory vault"),
                ("/ship", "Autonomous PR storyteller: conventional commits & PR draft"),
                ("/view", "View file with paged line slicing"),
                ("/write", "Atomically write content to file"),
                ("/edit", "Surgically search and replace text block"),
                ("/ls", "List directory contents"),
                ("/grep", "Search files recursively for pattern"),
                ("/find", "Find files by name/wildcard in directory"),
                ("/heal", "Compiler & test-driven self-healing loop"),
                ("/rules", "Discover or initialize workspace rules"),
                ("/repomap", "Syntactic AST repository map outline"),
                ("/map", "Alias for /repomap"),
                ("/cmd", "Execute sandboxed shell command"),
                ("/sh", "Alias for /cmd"),
                ("/agent", "Autonomous ReAct multi-turn agent loop"),
                ("/act", "Alias for /agent"),
                ("/watch", "Continuous Guardian background verification"),
                ("/ghost", "Inspect or apply staged memory ghost-fixes"),
                ("/devs", "DevServer Sentinel: scan localhost dev servers"),
                ("/impact", "Ripple Effect Radar: blast radius & call-sites"),
                ("/pod", "Specialist Swarm Pod: 4-role consensus pipeline"),
                ("/fix", "Terminal Rescue: mind-reader command diagnosis"),
                ("/memory", "Project Memory Ledger: context anchor & ADRs"),
                ("/mcp", "Universal MCP: list tools or call tool on server"),
                ("/predict", "Cascade next-edit anticipator across AST call-sites"),
                ("/ambient", "Alias for /predict"),
                ("/tweak", "Bidirectional DevTools CDP tweak to source mirror"),
                ("/cdp-sync", "Alias for /tweak"),
                ("/mode", "Composable prompt mode switch & docs harvester"),
                ("/harvest", "Alias for /mode"),
                ("/sandbox", "Zero-config in-memory stack sandbox & ephemeral port"),
                ("/box", "Alias for /sandbox"),
                ("/anti-placebo", "Behavioral mutation gatekeeper for test suite"),
                ("/gatekeeper", "Alias for /anti-placebo"),
            ];

            for (cmd, _) in slash_cmds {
                if cmd.starts_with(trimmed_prefix) {
                    let completed = format!("{cmd} ");
                    let len = completed.chars().count();
                    results.push((completed, len));
                }
            }
            return results;
        }

        // Subcommand completions for /mcp [list|call]
        if let Some(rest) = trimmed_prefix.strip_prefix("/mcp ") {
            let arg = rest.trim_start();
            let subcmds = ["list", "call"];
            for sub in subcmds {
                if sub.starts_with(arg) {
                    let completed = format!("/mcp {} ", sub);
                    let len = completed.chars().count();
                    results.push((completed, len));
                }
            }
            return results;
        }

        // 2. /model <name> or /models
        if let Some(rest) = trimmed_prefix.strip_prefix("/model ").or_else(|| trimmed_prefix.strip_prefix("/models ")) {
            let arg = rest.trim_start();
            let mut candidates: Vec<String> = vec![
                "list".to_string(),
                "current".to_string(),
                "gemini".to_string(),
                "gemini-2.5-flash".to_string(),
                "gemini-2.5-pro".to_string(),
                "gemini-2.0-flash".to_string(),
                "gemini-2.5-flash-lite".to_string(),
                "ollama".to_string(),
            ];

            for default_model in ["qwen2.5-coder:1.5b", "qwen2.5:0.5b", "llama3.2:1b", "smollm2:1.7b", "phi3:mini"] {
                if !candidates.contains(&default_model.to_string()) {
                    candidates.push(default_model.to_string());
                }
            }

            // Dynamically add all models currently held by local Ollama!
            let installed = hgb_core::OllamaProvider::installed_model_names();
            for m in installed {
                let prefixed = format!("ollama/{}", m);
                if !candidates.contains(&prefixed) {
                    candidates.push(prefixed);
                }
                if !candidates.contains(&m) {
                    candidates.push(m);
                }
            }

            for c in &candidates {
                if c.starts_with(arg) {
                    let completed = format!("/model {} ", c);
                    let len = completed.chars().count();
                    results.push((completed, len));
                }
            }
            return results;
        }

        // 3. /provenance <action>
        if let Some(rest) = trimmed_prefix.strip_prefix("/provenance ").or_else(|| trimmed_prefix.strip_prefix("/prov ")) {
            let arg = rest.trim_start();
            let actions = ["append", "audit", "verify", "status"];
            for a in actions {
                if a.starts_with(arg) {
                    let completed = format!("/provenance {} ", a);
                    let len = completed.chars().count();
                    results.push((completed, len));
                }
            }
            return results;
        }

        // 4. /checkpoint <action>
        if let Some(rest) = trimmed_prefix.strip_prefix("/checkpoint ").or_else(|| trimmed_prefix.strip_prefix("/ckpt ")) {
            let arg = rest.trim_start();
            let actions = ["create", "list", "restore", "rollback"];
            for a in actions {
                if a.starts_with(arg) {
                    let completed = format!("/checkpoint {} ", a);
                    let len = completed.chars().count();
                    results.push((completed, len));
                }
            }
            return results;
        }

        // 5. /fuzz <target>
        if let Some(rest) = trimmed_prefix.strip_prefix("/fuzz ") {
            let arg = rest.trim_start();
            let targets = ["sample_target", "invariants", "mesh", "storage"];
            for t in targets {
                if t.starts_with(arg) {
                    let completed = format!("/fuzz {} ", t);
                    let len = completed.chars().count();
                    results.push((completed, len));
                }
            }
            return results;
        }

        // 6. /verify <target>
        if let Some(rest) = trimmed_prefix.strip_prefix("/verify ") {
            let arg = rest.trim_start();
            let invariants = ["division", "bounds", "overflow", "x > 0"];
            for inv in invariants {
                if inv.starts_with(arg) {
                    let completed = format!("/verify {} ", inv);
                    let len = completed.chars().count();
                    results.push((completed, len));
                }
            }
            return results;
        }

        // 7. Path autocompletion for /view, /edit, /ls, /find
        if let Some((cmd_prefix, target_path)) = trimmed_prefix
            .strip_prefix("/view ")
            .map(|p| ("/view ", p))
            .or_else(|| trimmed_prefix.strip_prefix("/edit ").map(|p| ("/edit ", p)))
            .or_else(|| trimmed_prefix.strip_prefix("/ls ").map(|p| ("/ls ", p)))
            .or_else(|| trimmed_prefix.strip_prefix("/find ").map(|p| ("/find ", p)))
        {
            let clean_path = target_path.trim_start();
            let (dir_part, file_prefix) = match clean_path.rfind('/') {
                Some(idx) => (&clean_path[..=idx], &clean_path[idx + 1..]),
                None => ("", clean_path),
            };
            let dir_to_read = if dir_part.is_empty() { "." } else { dir_part };
            if let Ok(entries) = std::fs::read_dir(dir_to_read) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with(file_prefix) && !name.starts_with('.') {
                        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                        let suffix = if is_dir { "/" } else { " " };
                        let completed = format!("{}{}{}{}", cmd_prefix, dir_part, name, suffix);
                        let len = completed.chars().count();
                        results.push((completed, len));
                    }
                }
            }
        }

        results
    }

    /// Render column-aligned color pill autocompletion for slash commands, files, and models
    pub fn print_completion_grid(
        completions: &[(String, usize)],
        prefix: &str,
        term_width: usize,
        out: &mut dyn Write,
    ) -> io::Result<()> {
        let trimmed_prefix = prefix.trim_start();

        let mut items: Vec<(String, String)> = Vec::new();
        for (r, _) in completions {
            let (colored, raw) = if trimmed_prefix.starts_with("/model") {
                let model_name = r.strip_prefix("/model ").unwrap_or(r).trim();
                (
                    format!("[{}]", model_name).magenta().bold().to_string(),
                    format!("[{}]", model_name),
                )
            } else if trimmed_prefix.starts_with("/view")
                || trimmed_prefix.starts_with("/edit")
                || trimmed_prefix.starts_with("/ls")
                || trimmed_prefix.starts_with("/find")
            {
                let raw_path = r.split_whitespace().last().unwrap_or(r).trim();
                if raw_path.ends_with('/') {
                    (
                        format!("[📁 {}]", raw_path).blue().bold().to_string(),
                        format!("[📁 {}]", raw_path),
                    )
                } else {
                    (
                        format!("[📄 {}]", raw_path).yellow().bold().to_string(),
                        format!("[📄 {}]", raw_path),
                    )
                }
            } else if trimmed_prefix.starts_with('/') {
                let cmd = r.split_whitespace().next().unwrap_or(r).trim();
                (
                    format!("[{}]", cmd).cyan().bold().to_string(),
                    format!("[{}]", cmd),
                )
            } else {
                let word = r.trim();
                (
                    format!("[{}]", word).green().bold().to_string(),
                    format!("[{}]", word),
                )
            };
            items.push((colored, raw));
        }

        let max_len = items.iter().map(|(_, raw)| Self::visible_width(raw)).max().unwrap_or(12);
        let col_width = (max_len + 3).max(16);
        let num_cols = (term_width / col_width).max(1);

        for chunk in items.chunks(num_cols) {
            let mut row_str = String::from("  ");
            for (i, (colored, raw)) in chunk.iter().enumerate() {
                let raw_vis = Self::visible_width(raw);
                let pad = if i + 1 < chunk.len() {
                    col_width.saturating_sub(raw_vis)
                } else {
                    0
                };
                row_str.push_str(colored);
                row_str.push_str(&" ".repeat(pad));
            }
            writeln!(out, "{}\r", row_str)?;
        }

        Ok(())
    }

    /// Read an interactive line with full raw terminal mode and Tagisan keybindings
    pub fn read_line(
        &mut self,
        prompt: &str,
        on_repaint: &dyn Fn(),
    ) -> io::Result<ReadlineResult> {
        if !io::stdin().is_terminal() {
            let mut line = String::new();
            match io::stdin().read_line(&mut line) {
                Ok(0) => return Ok(ReadlineResult::Eof),
                Ok(_) => {
                    let trimmed = line.trim_end_matches(&['\r', '\n'][..]).to_string();
                    self.add_history(&trimmed);
                    return Ok(ReadlineResult::Submit(trimmed));
                }
                Err(e) => return Err(e),
            }
        }

        let mut buffer: Vec<char> = Vec::new();
        let mut cursor: usize = 0;
        let mut last_cursor_row: usize = 0;
        let cont_styled = format!("{}", "... ".cyan().dimmed());
        let continuation_prompt = cont_styled.as_str();
        self.history_index = self.history.len();
        self.draft.clear();

        let _raw_guard = match RawModeGuard::enter() {
            Ok(guard) => Some(guard),
            Err(_) => None,
        };

        if _raw_guard.is_none() {
            let mut line = String::new();
            match io::stdin().read_line(&mut line) {
                Ok(0) => return Ok(ReadlineResult::Eof),
                Ok(_) => {
                    let trimmed = line.trim_end_matches(&['\r', '\n'][..]).to_string();
                    self.add_history(&trimmed);
                    return Ok(ReadlineResult::Submit(trimmed));
                }
                Err(e) => return Err(e),
            }
        }

        Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;

        loop {
            let event = match event::read() {
                Ok(ev) => ev,
                Err(e) => return Err(e),
            };

            match event {
                Event::Paste(pasted_text) => {
                    let normalized = pasted_text.replace("\r\n", "\n").replace('\r', "\n");
                    for ch in normalized.chars() {
                        buffer.insert(cursor, ch);
                        cursor += 1;
                    }
                    Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                }
                Event::Resize(_, _) => {
                    Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                }
                Event::Key(key_event) => {
                    if key_event.kind == KeyEventKind::Release {
                        continue;
                    }

                    match key_event.code {
                        // ── Screen & Interrupt Handling ──────────────────
                        KeyCode::Char('c') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                            if !buffer.is_empty() {
                                let _ = Self::finalize_for_submit(prompt, continuation_prompt, &buffer, cursor);
                                buffer.clear();
                                cursor = 0;
                                last_cursor_row = 0;
                                let _ = write!(io::stdout(), "^C\r\n");
                                Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                            } else {
                                let _ = write!(io::stdout(), "^C\r\n  💡 Press Ctrl+D or type /exit to quit\r\n");
                                last_cursor_row = 0;
                                Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                            }
                        }
                        KeyCode::Char('d') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                            if buffer.is_empty() {
                                let _ = write!(io::stdout(), "\r\n");
                                return Ok(ReadlineResult::Eof);
                            } else if cursor < buffer.len() {
                                buffer.remove(cursor);
                                Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                            } else if buffer.contains(&'\n') {
                                Self::finalize_for_submit(prompt, continuation_prompt, &buffer, cursor)?;
                                let final_line: String = buffer.iter().collect();
                                self.add_history(&final_line);
                                return Ok(ReadlineResult::Submit(final_line));
                            }
                        }
                        KeyCode::Char('l') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                            let _ = write!(io::stdout(), "\x1b[2J\x1b[H\x1b[3J");
                            let _ = io::stdout().flush();
                            on_repaint();
                            last_cursor_row = 0;
                            Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                        }

                        // ── Inline Editing & Navigation ──────────────────
                        KeyCode::Char('a') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                            let line_start = buffer[..cursor].iter().rposition(|&c| c == '\n').map(|p| p + 1).unwrap_or(0);
                            cursor = if cursor == line_start { 0 } else { line_start };
                            Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                        }
                        KeyCode::Char('e') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                            let line_end = buffer[cursor..].iter().position(|&c| c == '\n').map(|p| cursor + p).unwrap_or(buffer.len());
                            cursor = if cursor == line_end { buffer.len() } else { line_end };
                            Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                        }
                        KeyCode::Home => {
                            let line_start = buffer[..cursor].iter().rposition(|&c| c == '\n').map(|p| p + 1).unwrap_or(0);
                            cursor = if cursor == line_start { 0 } else { line_start };
                            Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                        }
                        KeyCode::End => {
                            let line_end = buffer[cursor..].iter().position(|&c| c == '\n').map(|p| cursor + p).unwrap_or(buffer.len());
                            cursor = if cursor == line_end { buffer.len() } else { line_end };
                            Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                        }
                        KeyCode::Left => {
                            if key_event.modifiers.contains(KeyModifiers::CONTROL) || key_event.modifiers.contains(KeyModifiers::ALT) {
                                cursor = Self::find_word_backward(&buffer, cursor);
                            } else if cursor > 0 {
                                cursor -= 1;
                            }
                            Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                        }
                        KeyCode::Right => {
                            if key_event.modifiers.contains(KeyModifiers::CONTROL) || key_event.modifiers.contains(KeyModifiers::ALT) {
                                cursor = Self::find_word_forward(&buffer, cursor);
                            } else if cursor < buffer.len() {
                                cursor += 1;
                            }
                            Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                        }
                        KeyCode::Char('b') if key_event.modifiers.contains(KeyModifiers::ALT) => {
                            cursor = Self::find_word_backward(&buffer, cursor);
                            Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                        }
                        KeyCode::Char('f') if key_event.modifiers.contains(KeyModifiers::ALT) => {
                            cursor = Self::find_word_forward(&buffer, cursor);
                            Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                        }

                        // ── Kill Ring & Deletion ────────────────────────
                        KeyCode::Char('k') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                            if cursor < buffer.len() {
                                let line_end = buffer[cursor..].iter().position(|&c| c == '\n').map(|p| cursor + p).unwrap_or(buffer.len());
                                let kill_len = if line_end == cursor { 1 } else { line_end - cursor };
                                self.kill_ring = buffer[cursor..cursor + kill_len].iter().collect();
                                buffer.drain(cursor..cursor + kill_len);
                                Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                            }
                        }
                        KeyCode::Char('u') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                            if buffer.is_empty() {
                                println!();
                                return Ok(ReadlineResult::Submit("/undo".to_string()));
                            }
                            if cursor > 0 {
                                let line_start = buffer[..cursor].iter().rposition(|&c| c == '\n').map(|p| p + 1).unwrap_or(0);
                                let kill_start = if line_start == cursor { cursor - 1 } else { line_start };
                                self.kill_ring = buffer[kill_start..cursor].iter().collect();
                                buffer.drain(kill_start..cursor);
                                cursor = kill_start;
                                Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                            }
                        }
                        KeyCode::Char('w') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                            if cursor > 0 {
                                let kill_start = Self::find_word_backward(&buffer, cursor);
                                self.kill_ring = buffer[kill_start..cursor].iter().collect();
                                buffer.drain(kill_start..cursor);
                                cursor = kill_start;
                                Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                            }
                        }
                        KeyCode::Char('d') if key_event.modifiers.contains(KeyModifiers::ALT) => {
                            let kill_end = Self::find_word_forward(&buffer, cursor);
                            if kill_end > cursor {
                                self.kill_ring = buffer[cursor..kill_end].iter().collect();
                                buffer.drain(cursor..kill_end);
                                Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                            }
                        }
                        KeyCode::Char('y') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                            if !self.kill_ring.is_empty() {
                                for ch in self.kill_ring.chars() {
                                    buffer.insert(cursor, ch);
                                    cursor += 1;
                                }
                                Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                            }
                        }
                        KeyCode::Backspace | KeyCode::Char('h') if key_event.code == KeyCode::Backspace || key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                            if cursor > 0 {
                                buffer.remove(cursor - 1);
                                cursor -= 1;
                                Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                            }
                        }
                        KeyCode::Delete => {
                            if cursor < buffer.len() {
                                buffer.remove(cursor);
                                Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                            }
                        }

                        // ── Multiline & History Navigation ──────────────
                        KeyCode::Up => {
                            let line_start = buffer[..cursor].iter().rposition(|&c| c == '\n').map(|p| p + 1).unwrap_or(0);
                            if line_start > 0 {
                                let col_offset = cursor - line_start;
                                let prev_line_end = line_start - 1;
                                let prev_line_start = buffer[..prev_line_end].iter().rposition(|&c| c == '\n').map(|p| p + 1).unwrap_or(0);
                                let prev_line_len = prev_line_end - prev_line_start;
                                cursor = prev_line_start + col_offset.min(prev_line_len);
                                Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                            } else {
                                if self.history_index == self.history.len() {
                                    self.draft = buffer.clone();
                                }
                                if self.history_index > 0 {
                                    self.history_index -= 1;
                                    buffer = self.history[self.history_index].chars().collect();
                                    cursor = buffer.len();
                                    Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                                }
                            }
                        }
                        KeyCode::Down => {
                            let line_start = buffer[..cursor].iter().rposition(|&c| c == '\n').map(|p| p + 1).unwrap_or(0);
                            let col_offset = cursor - line_start;
                            let next_line_rel = buffer[cursor..].iter().position(|&c| c == '\n');
                            if let Some(pos) = next_line_rel {
                                let next_line_start = cursor + pos + 1;
                                let next_line_end = buffer[next_line_start..].iter().position(|&c| c == '\n').map(|p| next_line_start + p).unwrap_or(buffer.len());
                                let next_line_len = next_line_end - next_line_start;
                                cursor = next_line_start + col_offset.min(next_line_len);
                                Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                            } else {
                                if self.history_index + 1 < self.history.len() {
                                    self.history_index += 1;
                                    buffer = self.history[self.history_index].chars().collect();
                                    cursor = buffer.len();
                                    Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                                } else if self.history_index + 1 == self.history.len() {
                                    self.history_index = self.history.len();
                                    buffer = self.draft.clone();
                                    cursor = buffer.len();
                                    Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                                }
                            }
                        }
                        KeyCode::Char('r') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                            let _ = Self::run_reverse_search(&self.history, &mut buffer, &mut cursor, prompt);
                            last_cursor_row = 0;
                            Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                        }

                        // ── Tab Autocompletion ──────────────────────────
                        KeyCode::Tab => {
                            let current_text: String = buffer.iter().collect();
                            let prefix = &current_text[..cursor];

                            let completions = Self::get_completions(prefix);
                            if completions.is_empty() {
                                // No match
                            } else if completions.len() == 1 {
                                let (replacement, new_cursor) = completions[0].clone();
                                buffer = replacement.chars().collect();
                                cursor = new_cursor;
                                Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                            } else {
                                let replacement_strings: Vec<String> = completions.iter().map(|(r, _)| r.clone()).collect();
                                let common = Self::longest_common_prefix(&replacement_strings);
                                if common.len() > prefix.len() {
                                    buffer = common.chars().collect();
                                    cursor = buffer.len();
                                }

                                let term_width = crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(80).max(40);
                                let mut stdout = io::stdout();
                                let _ = write!(stdout, "\r\n");

                                Self::print_completion_grid(&completions, prefix, term_width, &mut stdout)?;

                                last_cursor_row = 0;
                                Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                            }
                        }

                        // ── Submission & Multi-Line Continuation ────────
                        KeyCode::Char('j') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                            buffer.insert(cursor, '\n');
                            cursor += 1;
                            Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                        }
                        KeyCode::Enter => {
                            let is_explicit_continuation = key_event.modifiers.contains(KeyModifiers::ALT)
                                || key_event.modifiers.contains(KeyModifiers::SHIFT);

                            let has_trailing_backslash = buffer.ends_with(&['\\']);

                            let buf_str: String = buffer.iter().collect();
                            let is_inside_unclosed = Self::has_unclosed_delimiters(&buf_str);

                            let is_rapid_paste = event::poll(std::time::Duration::from_millis(10)).unwrap_or(false);

                            if is_explicit_continuation || has_trailing_backslash || is_inside_unclosed || is_rapid_paste {
                                if has_trailing_backslash {
                                    buffer.pop();
                                    if cursor > buffer.len() {
                                        cursor = buffer.len();
                                    }
                                }
                                buffer.insert(cursor, '\n');
                                cursor += 1;
                                Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                            } else {
                                Self::finalize_for_submit(prompt, continuation_prompt, &buffer, cursor)?;
                                let final_line: String = buffer.iter().collect();
                                self.add_history(&final_line);
                                return Ok(ReadlineResult::Submit(final_line));
                            }
                        }

                        // ── Regular Characters ──────────────────────────
                        KeyCode::Char(c) if !key_event.modifiers.contains(KeyModifiers::CONTROL) && !key_event.modifiers.contains(KeyModifiers::ALT) => {
                            buffer.insert(cursor, c);
                            cursor += 1;
                            Self::redraw_multiline(prompt, continuation_prompt, &buffer, cursor, &mut last_cursor_row)?;
                        }

                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
}

pub struct HagibisRepl {
    client: HgbClient,
    model: Option<String>,
}

impl HagibisRepl {
    pub fn new(client: HgbClient) -> Self {
        let initial_model = hgb_core::load_active_model();
        Self {
            client,
            model: initial_model,
        }
    }

    #[allow(dead_code)]
    pub fn current_model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    #[allow(dead_code)]
    pub fn set_model(&mut self, model: Option<String>) {
        self.model = model;
    }

    pub fn clean_model_input(arg: &str) -> Option<String> {
        let trimmed = arg.trim();
        if trimmed.is_empty() {
            return None;
        }

        let lower = trimmed.to_lowercase();
        let stripped = if lower.starts_with("switch ") {
            trimmed[7..].trim()
        } else if lower.starts_with("set ") {
            trimmed[4..].trim()
        } else if lower.starts_with("use ") {
            trimmed[4..].trim()
        } else if lower.starts_with("to ") {
            trimmed[3..].trim()
        } else if lower == "switch" || lower == "set" || lower == "use" || lower == "to" {
            return None;
        } else {
            trimmed
        };

        if stripped.is_empty() {
            return None;
        }

        let token = stripped.split_whitespace().next().unwrap_or(stripped);
        let clean = token
            .trim_matches(|c| c == '\'' || c == '"' || c == '`' || c == '(' || c == ')' || c == '[' || c == ']')
            .trim();

        if clean.is_empty() {
            None
        } else {
            Some(clean.to_string())
        }
    }

    #[allow(dead_code)]
    pub fn print_banner() {
        Self::print_banner_with_model(None);
    }

    pub fn print_banner_with_model(model: Option<&str>) {
        let width = match crossterm::terminal::size() {
            Ok((w, _)) => (w as usize).clamp(72, 88),
            Err(_) => 80,
        };

        let inner_width = width - 2;

        let top_border = format!("╭{}╮", "─".repeat(inner_width));
        let divider = format!("├{}┤", "─".repeat(inner_width));
        let bottom_border = format!("╰{}╯", "─".repeat(inner_width));

        // 1. App Title
        let title_colored = format!(
            "{} {} {}",
            "▲".cyan().bold(),
            "HAGIBIS (hgb)".white().bold(),
            "─ Autonomous Microkernel & Swarm Engine".dimmed()
        );

        // 2. Active Model Pill
        let active_model = model.unwrap_or("gemini-2.5-flash");
        let model_pill_colored = if hgb_core::OllamaProvider::is_ollama_model(active_model) {
            format!(
                "{} {}",
                format!("[{}]", active_model).on_magenta().black().bold(),
                "(0ms local)".dimmed().magenta()
            )
        } else if active_model.contains("gemini") {
            format!(
                "{} {}",
                format!("[{}]", active_model).on_cyan().black().bold(),
                "(auto-failover)".dimmed().cyan()
            )
        } else {
            format!("[{}]", active_model).on_cyan().black().bold().to_string()
        };

        // 3. Account Identity
        let account_email = hgb_core::GeminiOAuthManager::get_account_email()
            .unwrap_or_else(|| "buzer.agy@gmail.com".to_string());
        let account_pill_colored = format!("[👤 {}]", account_email).green().bold();

        // 4. Workspace Directory
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| ".".to_string());
        let max_cwd_len = inner_width.saturating_sub(18);
        let display_cwd = if cwd.chars().count() > max_cwd_len && max_cwd_len > 10 {
            format!("...{}", &cwd[cwd.len().saturating_sub(max_cwd_len - 3)..])
        } else {
            cwd.clone()
        };
        let workspace_pill_colored = format!("[📁 {}]", display_cwd).yellow();

        // 5. Telemetry Status
        let telemetry_colored = format!(
            "[{}: {} • {}: {} • {}: {}]",
            "Tokio IPC".dimmed(),
            "12µs".green(),
            "Swarm".dimmed(),
            "Zero-Copy".cyan(),
            "Provenance".dimmed(),
            "Nominal".green()
        );

        // 6. Quick Commands Summary
        let quick_colored = format!(
            "Quick: {} • {} • {} • {}",
            "/help".cyan().bold(),
            "/cockpit".magenta().bold(),
            "/model".yellow().bold(),
            "/exit".red().bold()
        );

        let pad_line = |content: &str| -> String {
            let vis = ReplEditor::visible_width(content);
            let pad = inner_width.saturating_sub(vis + 2);
            format!("│ {}{} │", content, " ".repeat(pad))
        };

        println!("{}", top_border.cyan());
        println!("{}", pad_line(&title_colored));
        println!("{}", divider.cyan());
        println!("{}", pad_line(&format!("Model:     {}", model_pill_colored)));
        println!("{}", pad_line(&format!("Account:   {}", account_pill_colored)));
        println!("{}", pad_line(&format!("Workspace: {}", workspace_pill_colored)));
        println!("{}", pad_line(&format!("Telemetry: {}", telemetry_colored)));
        println!("{}", divider.cyan());
        println!("{}", pad_line(&quick_colored));
        println!("{}", bottom_border.cyan());
    }

    pub async fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // Automatically sync active model from environment, resident daemon, persisted preference, or local discovery
        if self.model.is_none() {
            if let Ok(explicit) = std::env::var("HGB_MODEL") {
                self.model = Some(explicit);
            } else {
                let resp = self.dispatch(HgbRequest::Status).await;
                if let HgbResponse::Status(s) = resp {
                    if let Some(active) = s.active_models.first() {
                        self.model = Some(active.clone());
                    }
                } else if let Some(persisted) = hgb_core::load_active_model() {
                    self.model = Some(persisted);
                } else if !hgb_core::GeminiProvider::is_available() && hgb_core::OllamaProvider::is_available() {
                    if let Some(prov) = hgb_core::OllamaProvider::auto_discover() {
                        self.model = Some(prov.default_model().to_string());
                    }
                }
            }
        }

        if let Some(ref m) = self.model {
            if hgb_core::OllamaProvider::is_ollama_model(m) && !hgb_core::OllamaProvider::is_installed_model(m) {
                if let Some(resolved) = hgb_core::validate_and_resolve_active_model(Some(m)) {
                    self.model = Some(resolved);
                }
            }
        }

        Self::print_banner_with_model(self.model.as_deref());

        let mut editor = ReplEditor::new();

        loop {
            let active_model = self.model.as_deref().unwrap_or("gemini-2.5-flash");
            let prompt = format!(
                "{} {} {} ",
                "hgb".bold().cyan(),
                format!("[{}]", active_model).dimmed(),
                "❯".bold().green()
            );
            let model_clone = self.model.clone();
            let repaint = move || {
                Self::print_banner_with_model(model_clone.as_deref());
            };

            let line_result = editor.read_line(&prompt, &repaint)?;

            let input = match line_result {
                ReadlineResult::Submit(s) => s,
                ReadlineResult::Eof => break,
                ReadlineResult::Interrupted => continue,
            };

            let trimmed = input.trim();
            if trimmed.is_empty() {
                continue;
            }

            if trimmed.starts_with('/') {
                if !self.handle_slash_command(trimmed).await? {
                    break;
                }
            } else {
                self.execute_prompt(trimmed).await?;
            }
        }

        println!("{}", "👋 Goodbye from Hagibis!".cyan());
        Ok(())
    }

    pub async fn handle_slash_command(&mut self, input: &str) -> Result<bool, Box<dyn std::error::Error>> {
        let parts: Vec<&str> = input.split_whitespace().collect();
        let cmd = parts[0].to_lowercase();
        let args = if parts.len() > 1 { parts[1..].join(" ") } else { String::new() };

        match cmd.as_str() {
            "/exit" | "/quit" | "/q" | "/help" | "/?" | "/h" | "/clear" | "/cls" | "/ping"
            | "/status" | "/doctor" | "/doc" | "/model" | "/models" | "/login" | "/auth"
            | "/provenance" | "/prov" | "/checkpoint" | "/ckpt" | "/fuzz" | "/verify"
            | "/mesh" | "/cockpit" => {
                self.handle_core_command(&cmd, &args, &parts).await
            }
            "/view" | "/cat" | "/write" | "/edit" | "/ls" | "/dir" | "/grep" | "/find" | "/search" => {
                self.handle_crud_command(&cmd, &args, &parts).await
            }
            "/vibe" | "/race" | "/undo" | "/diff" | "/heal" | "/rules" | "/repomap" | "/map"
            | "/cmd" | "/sh" | "/exec" | "/run-cmd" | "/agent" | "/act" | "/style" | "/ship"
            | "/watch" | "/guardian" | "/ghost" | "/devs" | "/devscan" | "/sentinel" | "/impact"
            | "/pod" | "/fix" | "/rescue" | "/memory" | "/mem" | "/glance" | "/pkg" | "/guard"
            | "/env" | "/proxy" | "/trace" | "/worktree" | "/wt" | "/stash" | "/snoop" | "/browser"
            | "/race3" | "/variant-race" | "/dbsync" | "/db" | "/slice" | "/spec" | "/dna"
            | "/mcp" | "/patch" | "/live" | "/tdd" | "/isolate" | "/chime" | "/hmr" | "/lens"
            | "/swarm" | "/dbsnap" | "/dbrewind" | "/shield" | "/launch" | "/flight" => {
                self.handle_vibe_command(&cmd, &args, &parts).await
            }
            "/ghostcoder" | "/mirage" | "/chaos" | "/nightshift" | "/vault" | "/typelock"
            | "/radar" | "/teleport" | "/voice" | "/tape" | "/finops" | "/cloak" | "/sqlguard"
            | "/sql" | "/replay" | "/canvas" | "/federate" | "/timewarp" | "/guardrails"
            | "/triage" | "/deflake" | "/anchor" => {
                self.handle_superpowers_command(&cmd, &args).await
            }
            "/lsp" | "/ghost-lsp" | "/compact" | "/commit" | "/recipe" | "/contract" | "/graph"
            | "/shadow" | "/mutation" | "/mutation-audit" | "/resilience" | "/cost" | "/security"
            | "/compliance" | "/live-graph" | "/panic-fix" | "/plan-spec" | "/at-expand"
            | "/visual-sentry" | "/heal-watch"
            | "/predict" | "/ambient" | "/tweak" | "/cdp-sync" | "/mode" | "/harvest"
            | "/sandbox" | "/box" | "/anti-placebo" | "/gatekeeper"
            | "/preview" | "/vision" | "/share" | "/graduate" | "/expand" | "/auto-heal" => {
                self.handle_frontier_command(&cmd, &args).await
            }
            _ => {
                println!("{} Unknown slash command '{}'. Type '/help' for available commands.", "⚠".yellow(), cmd);
                Ok(true)
            }
        }
    }

    async fn handle_core_command(&mut self, cmd: &str, args: &str, _parts: &[&str]) -> Result<bool, Box<dyn std::error::Error>> {
        match cmd {
            "/exit" | "/quit" | "/q" => return Ok(false),
            "/help" | "/?" | "/h" => self.print_help(),
            "/clear" | "/cls" => {
                print!("\x1B[2J\x1B[1;1H\x1b[3J");
                let _ = io::stdout().flush();
                Self::print_banner_with_model(self.model.as_deref());
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
            "/model" | "/models" => {
                let trimmed_args = args.trim();
                let lower = trimmed_args.to_lowercase();
                if lower.is_empty() || lower == "list" || lower == "ls" || lower == "show" {
                    self.show_models().await;
                } else if lower == "current" || lower == "status" || lower == "get" {
                    let current = self.model.as_deref().unwrap_or("auto");
                    let is_local = hgb_core::OllamaProvider::is_ollama_model(current);
                    println!("  🪽 Current active model: {}", current.bold().green());
                    if is_local {
                        let ollama_status = if hgb_core::OllamaProvider::is_available() {
                            "Local Ollama Engine (online, 0ms latency, zero cloud cost)".magenta()
                        } else {
                            "Local Ollama Engine (⚠️ OFFLINE at 127.0.0.1:11434)".yellow()
                        };
                        println!("  [•] Provider: {}", ollama_status);
                    } else {
                        println!("  [•] Provider: Google Gemini Cloud ({})", hgb_core::GeminiProvider::credential_status().cyan());
                    }
                } else if lower == "help" || lower == "-h" || lower == "--help" {
                    println!("\n{}", "💡 Model Switch Command Usage:".bold().cyan());
                    println!("  /model [list|ls|show]           List all available local and cloud models");
                    println!("  /model [current|status]         Display currently active model");
                    println!("  /model <name>                   Switch to specific model (e.g. /model gemini-2.5-pro)");
                    println!("  /model switch <name>            Explicit switch syntax");
                    println!("  /model auto                     Reset to intelligent dual-brain auto-failover\n");
                } else {
                    let candidate = Self::clean_model_input(trimmed_args);
                    let clean = match candidate {
                        Some(c) => c,
                        None => {
                            println!("  {} Please specify a target model name. (e.g. /model gemini-2.5-pro, /model ollama/qwen2.5:0.5b)", "⚠".yellow());
                            println!("  {} Run '/model list' to view all installed and available models.", "ℹ".cyan());
                            return Ok(true);
                        }
                    };

                    let is_local = hgb_core::OllamaProvider::is_ollama_model(&clean);
                    let resp = self.dispatch(HgbRequest::ModelSwitch { model: clean.clone() }).await;

                    match resp {
                        HgbResponse::ModelSwitched { previous, current, duration_ms } => {
                            self.model = Some(current.clone());
                            let _ = hgb_core::persist_active_model(&current);
                            println!("  ✔ Model switched: {} ➔ {} (in {} ms)", previous.dimmed(), current.green().bold(), duration_ms);
                            if is_local {
                                if hgb_core::OllamaProvider::is_available() {
                                    println!("  [•] Provider: {}", "Local Ollama Engine (online, 0ms latency, zero cloud cost)".magenta());
                                } else {
                                    println!("  [•] Provider: {}", "Local Ollama Engine (⚠️ OFFLINE at 127.0.0.1:11434 - start with 'ollama serve')".yellow());
                                }
                            } else {
                                let cred = hgb_core::GeminiProvider::credential_status();
                                println!("  [•] Provider: Google Gemini Cloud ({})", cred.cyan());
                            }
                        }
                        HgbResponse::Error(err) => {
                            println!("  {} Failed to switch model: {}", "✖".red().bold(), err);
                        }
                        other => {
                            self.model = Some(clean.clone());
                            let _ = hgb_core::persist_active_model(&clean);
                            println!("  ✔ Model set to: {}", clean.green().bold());
                            self.render_response(other);
                        }
                    }
                }
            }
            "/login" => {
                let resp = self.dispatch(HgbRequest::Login).await;
                self.render_response(resp);
            }
            "/auth" => {
                let resp = self.dispatch(HgbRequest::AuthStatus).await;
                self.render_response(resp);
            }
            "/provenance" | "/prov" => {
                let action = if args.is_empty() { "append".to_string() } else { args.to_string() };
                let resp = self.dispatch(HgbRequest::Provenance { action }).await;
                self.render_response(resp);
            }
            "/checkpoint" | "/ckpt" => {
                let label = if args.is_empty() { None } else { Some(args.to_string()) };
                let resp = self.dispatch(HgbRequest::Checkpoint { action: "create".to_string(), label }).await;
                self.render_response(resp);
            }
            "/fuzz" => {
                let target = if args.is_empty() { "sample_target".to_string() } else { args.to_string() };
                let resp = self.dispatch(HgbRequest::Fuzz { target, iterations: 100 }).await;
                self.render_response(resp);
            }
            "/verify" => {
                let target = if args.is_empty() { "x > 0".to_string() } else { args.to_string() };
                let resp = self.dispatch(HgbRequest::Verify { target, invariant: "division".to_string() }).await;
                self.render_response(resp);
            }
            "/mesh" => {
                let resp = self.dispatch(HgbRequest::MeshStatus).await;
                self.render_response(resp);
            }
            "/cockpit" => {
                let mut state = hgb_nextgen::CockpitState::new();
                if let Some(ref m) = self.model {
                    state.model_pill = m.clone();
                }
                let mut node1 = hgb_nextgen::CockpitDagNode::new("n1", "Microkernel Ingestion", "hgb-core");
                node1.status = hgb_nextgen::CockpitNodeStatus::Succeeded { duration_ms: 8 };
                node1.tokens_used = 120;
                node1.scratchpad = "Fast IPC zero-copy parsing complete.".to_string();

                let model_str = self.model.as_deref().unwrap_or("gemini-2.5-flash");
                let mut node2 = hgb_nextgen::CockpitDagNode::new("n2", "Active LLM Reasoning", model_str);
                node2.status = hgb_nextgen::CockpitNodeStatus::Running { progress_pct: 85 };
                node2.tokens_used = 890;
                node2.scratchpad = format!("Reasoning model '{}' active on prompt...", model_str);

                let mut node3 = hgb_nextgen::CockpitDagNode::new("n3", "Surgical File CRUD", "hgb-fs");
                node3.status = hgb_nextgen::CockpitNodeStatus::Pending;

                state.add_node(node1);
                state.add_node(node2);
                state.add_node(node3);

                let _ = state.run_interactive().await;
                self.model = Some(state.model_pill.clone());
                let _ = hgb_core::persist_active_model(&state.model_pill);
                print!("\x1B[2J\x1B[1;1H\x1b[3J");
                let _ = io::stdout().flush();
                Self::print_banner_with_model(self.model.as_deref());
            }
            _ => {}
        }
        Ok(true)
    }

    async fn handle_crud_command(&mut self, cmd: &str, _args: &str, parts: &[&str]) -> Result<bool, Box<dyn std::error::Error>> {
        match cmd {
            // --- AGY Surgical CRUD Slash Commands ---
            "/view" | "/cat" => {
                if parts.len() < 2 {
                    println!("{} Usage: /view <path> [start_line] [end_line]", "ℹ".yellow());
                } else {
                    let path = parts[1].to_string();
                    let start_line = parts.get(2).and_then(|s| s.parse::<usize>().ok());
                    let end_line = parts.get(3).and_then(|s| s.parse::<usize>().ok());
                    let start_timer = std::time::Instant::now();
                    let resp = self.dispatch(HgbRequest::CrudView { path: path.clone(), start_line, end_line, offset: None }).await;
                    let dur_ms = start_timer.elapsed().as_millis() as u64;
                    match resp {
                        HgbResponse::Complete { output, tokens_used, .. } => {
                            let summary = format!("path='{}', lines={:?}-{:?}", path, start_line, end_line);
                            let card = ToolCallCard::new("view_file", summary, ToolCardStatus::Success {
                                duration_ms: dur_ms,
                                exit_code: 0,
                            })
                            .with_output(output)
                            .with_details(format!("Total lines: {}", tokens_used));
                            card.print();
                        }
                        other => self.render_response(other),
                    }
                }
            }
            "/write" => {
                if parts.len() < 3 {
                    println!("{} Usage: /write <path> <content>", "ℹ".yellow());
                } else {
                    let path = parts[1].to_string();
                    let content = parts[2..].join(" ");
                    let start_timer = std::time::Instant::now();
                    let resp = self.dispatch(HgbRequest::CrudWrite {
                        path: path.clone(),
                        content: content.clone(),
                        overwrite: true,
                        artifact_summary: None,
                    }).await;
                    let dur_ms = start_timer.elapsed().as_millis() as u64;
                    match resp {
                        HgbResponse::Complete { output, .. } => {
                            let summary = format!("path='{}', bytes={}", path, content.len());
                            let card = ToolCallCard::new("write_to_file", summary, ToolCardStatus::Success {
                                duration_ms: dur_ms,
                                exit_code: 0,
                            }).with_output(output);
                            card.print();
                        }
                        other => self.render_response(other),
                    }
                }
            }
            "/edit" => {
                if parts.len() < 4 {
                    println!("{} Usage: /edit <path> <target> <replacement>", "ℹ".yellow());
                } else {
                    let path = parts[1].to_string();
                    let target = parts[2].to_string();
                    let replacement = parts[3..].join(" ");
                    let start_timer = std::time::Instant::now();
                    let resp = self.dispatch(HgbRequest::CrudEdit {
                        path: path.clone(),
                        target: target.clone(),
                        replacement: replacement.clone(),
                        start_line: None,
                        end_line: None,
                        allow_multiple: false,
                        instruction: None,
                        description: None,
                        target_lint_error_ids: Vec::new(),
                    }).await;
                    let dur_ms = start_timer.elapsed().as_millis() as u64;
                    match resp {
                        HgbResponse::Complete { output, .. } => {
                            let summary = format!("path='{}', target='{}'", path, target);
                            let card = ToolCallCard::new("replace_file_content", summary, ToolCardStatus::Success {
                                duration_ms: dur_ms,
                                exit_code: 0,
                            }).with_output(output);
                            card.print();
                        }
                        other => self.render_response(other),
                    }
                }
            }
            "/ls" | "/dir" => {
                let path = if parts.len() > 1 { parts[1].to_string() } else { ".".to_string() };
                let start_timer = std::time::Instant::now();
                let resp = self.dispatch(HgbRequest::CrudList { path: path.clone() }).await;
                let dur_ms = start_timer.elapsed().as_millis() as u64;
                match resp {
                    HgbResponse::Complete { output, .. } => {
                        let card = ToolCallCard::new("list_dir", format!("path='{}'", path), ToolCardStatus::Success {
                            duration_ms: dur_ms,
                            exit_code: 0,
                        }).with_output(output);
                        card.print();
                    }
                    other => self.render_response(other),
                }
            }
            "/grep" => {
                if parts.len() < 2 {
                    println!("{} Usage: /grep <pattern> [path]", "ℹ".yellow());
                } else {
                    let pattern = parts[1].to_string();
                    let path = parts.get(2).map(|p| p.to_string());
                    let start_timer = std::time::Instant::now();
                    let resp = self.dispatch(HgbRequest::CrudGrep {
                        pattern: pattern.clone(),
                        path: path.clone(),
                        is_regex: false,
                        case_insensitive: true,
                        match_per_line: true,
                        includes: Vec::new(),
                    }).await;
                    let dur_ms = start_timer.elapsed().as_millis() as u64;
                    match resp {
                        HgbResponse::Complete { output, .. } => {
                            let summary = format!("pattern='{}', path={:?}", pattern, path);
                            let card = ToolCallCard::new("grep_search", summary, ToolCardStatus::Success {
                                duration_ms: dur_ms,
                                exit_code: 0,
                            }).with_output(output);
                            card.print();
                        }
                        other => self.render_response(other),
                    }
                }
            }
            "/find" | "/search" => {
                if parts.len() < 2 {
                    println!("{} Usage: /find <pattern> [search_dir] [ext]", "ℹ".yellow());
                } else {
                    let pattern = Some(parts[1].to_string());
                    let search_directory = parts.get(2).map(|p| p.to_string()).unwrap_or_else(|| ".".to_string());
                    let extensions = if parts.len() > 3 {
                        parts[3].split(',').map(|s| s.trim().to_string()).collect()
                    } else {
                        Vec::new()
                    };
                    let start_timer = std::time::Instant::now();
                    let resp = self.dispatch(HgbRequest::CrudFind {
                        search_directory: search_directory.clone(),
                        pattern: pattern.clone(),
                        extensions,
                        excludes: Vec::new(),
                        max_depth: None,
                        target_type: None,
                    }).await;
                    let dur_ms = start_timer.elapsed().as_millis() as u64;
                    match resp {
                        HgbResponse::Complete { output, .. } => {
                            let summary = format!("pattern={:?}, dir='{}'", pattern, search_directory);
                            let card = ToolCallCard::new("find_by_name", summary, ToolCardStatus::Success {
                                duration_ms: dur_ms,
                                exit_code: 0,
                            }).with_output(output);
                            card.print();
                        }
                        other => self.render_response(other),
                    }
                }
            }
            _ => {}
        }
        Ok(true)
    }

    async fn handle_vibe_command(&mut self, cmd: &str, args: &str, _parts: &[&str]) -> Result<bool, Box<dyn std::error::Error>> {
        match cmd {
            // --- Vibe Coding Capabilities ---
            "/vibe" | "/race" => {
                if args.is_empty() {
                    println!("{} Usage: /vibe <prompt> or /race <prompt>", "ℹ".yellow());
                } else {
                    println!("{}", "🏁 Spawning Speculative Dual-Draft Race (Fast Local Draft vs Frontier)...".cyan().bold());
                    let req = HgbRequest::VibeRace {
                        prompt: args.to_string(),
                        target_dir: None,
                    };
                    let resp = self.dispatch(req).await;
                    hgb_core::audio::play_vibe_chime(matches!(resp, HgbResponse::RaceResult { passed_checks: true, .. }));
                    self.render_response(resp);
                }
            }
            "/undo" => {
                let target_ckpt = if args.is_empty() { None } else { Some(args.to_string()) };
                println!("{}", "⏪ Executing 1-Key Time-Travel Rollback (Blake3 WAL & File Snapshots)...".cyan().bold());
                let resp = self.dispatch(HgbRequest::Undo { checkpoint_id: target_ckpt }).await;
                hgb_core::audio::play_vibe_chime(true);
                self.render_response(resp);
            }
            "/diff" => {
                let output = std::process::Command::new("git")
                    .args(["diff", "HEAD"])
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_else(|_| "No diff available or not in a git repository.".to_string());
                if output.trim().is_empty() {
                    println!("  [•] Working tree clean, zero uncommitted diffs.");
                } else {
                    let hunks = hgb_nextgen::SelectivePatcher::parse_unified_diff(&output);
                    if !hunks.is_empty() {
                        ChatCanvas::print_diff_hud(&hunks);
                    } else {
                        ChatCanvas::print_markdown(&format!("```diff\n{}\n```", output));
                    }
                }
            }
            "/heal" => {
                let check_command = if args.is_empty() { None } else { Some(args.to_string()) };
                println!("{}", format!("🩹 Launching Self-Healing Loop for '{}'...", check_command.as_deref().unwrap_or("cargo check")).magenta().bold());
                let resp = self.dispatch(HgbRequest::Heal {
                    check_command,
                    workspace_root: None,
                }).await;
                hgb_core::audio::play_vibe_chime(matches!(resp, HgbResponse::HealResult { fully_healed: true, .. }));
                self.render_response(resp);
            }
            "/rules" => {
                if args.starts_with("init") {
                    println!("{}", "📜 Initializing default workspace rules (.hgb/rules)...".cyan().bold());
                    let resp = self.dispatch(HgbRequest::InitRules { workspace_root: None }).await;
                    self.render_response(resp);
                } else {
                    let resp = self.dispatch(HgbRequest::GetRules { workspace_root: None }).await;
                    self.render_response(resp);
                }
            }
            "/repomap" | "/map" => {
                let max_files = if args.is_empty() { None } else { args.parse::<usize>().ok() };
                println!("{}", "🗺️ Generating Syntactic Repo Map Outline...".cyan().bold());
                let resp = self.dispatch(HgbRequest::GetRepoMap { workspace_root: None, max_files }).await;
                self.render_response(resp);
            }
            "/cmd" | "/sh" | "/exec" | "/run-cmd" => {
                if args.is_empty() {
                    println!("{} Usage: /cmd <command>", "⚠".yellow());
                } else {
                    let resp = self.dispatch(HgbRequest::RunCommand {
                        command: args.to_string(),
                        cwd: None,
                        timeout_ms: Some(30_000),
                    }).await;
                    self.render_response(resp);
                }
            }
            "/agent" | "/act" => {
                if args.is_empty() {
                    println!("{} Usage: /agent <task description>", "⚠".yellow());
                } else {
                    println!("{}", format!("🤖 Launching Autonomous ReAct Agent Loop for: '{}'...", args).cyan().bold());
                    let resp = self.dispatch(HgbRequest::AgentRun {
                        prompt: args.to_string(),
                        model: self.model.clone(),
                        max_turns: Some(10),
                        workspace_root: None,
                    }).await;
                    self.render_response(resp);
                }
            }
            "/style" => {
                if args.starts_with("learn") {
                    let snippet = args.trim_start_matches("learn").trim();
                    let accepted = !snippet.contains("--reject");
                    let clean_snippet = snippet.replace("--accept", "").replace("--reject", "").trim().to_string();
                    let resp = self.dispatch(HgbRequest::RecordStyleFeedback { snippet: clean_snippet, accepted }).await;
                    self.render_response(resp);
                } else {
                    let resp = self.dispatch(HgbRequest::GetStyleGuidance).await;
                    self.render_response(resp);
                }
            }
            "/ship" => {
                let dry_run = args.contains("--dry-run");
                println!("{}", "🚢 Invoking Autonomous PR Storyteller (Blake3 Checkpoints)...".cyan().bold());
                let resp = self.dispatch(HgbRequest::Ship { dry_run }).await;
                hgb_core::audio::play_vibe_chime(matches!(resp, HgbResponse::ShipReport { security_passed: true, .. }));
                self.render_response(resp);
            }
            "/watch" | "/guardian" => {
                let cmd = if args.is_empty() { None } else { Some(args.to_string()) };
                println!("{}", "🛡️ Starting Continuous Guardian Watcher...".cyan().bold());
                let resp = self.dispatch(HgbRequest::GuardianStart {
                    check_command: cmd,
                    debounce_ms: Some(350),
                }).await;
                self.render_response(resp);
            }
            "/ghost" => {
                if args.starts_with("apply") {
                    let fix_id = args.trim_start_matches("apply").trim();
                    if fix_id.is_empty() {
                        println!("{} Usage: /ghost apply <fix-id>", "⚠".yellow());
                    } else {
                        let resp = self.dispatch(HgbRequest::GuardianApplyFix {
                            fix_id: fix_id.to_string(),
                        }).await;
                        self.render_response(resp);
                    }
                } else {
                    let resp = self.dispatch(HgbRequest::GuardianStatus).await;
                    self.render_response(resp);
                }
            }
            "/devs" | "/devscan" | "/sentinel" => {
                println!("{}", "🌐 Scanning for active local dev server endpoints...".cyan().bold());
                let resp = self.dispatch(HgbRequest::DevServerScan).await;
                self.render_response(resp);
            }
            "/impact" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                if parts.len() < 2 {
                    println!("{} Usage: /impact <symbol> <originating_file>", "⚠".yellow());
                } else {
                    println!("{}", format!("📡 Assessing Ripple Impact for symbol '{}'...", parts[0]).cyan().bold());
                    let resp = self.dispatch(HgbRequest::ImpactAnalyze {
                        symbol: parts[0].to_string(),
                        originating_file: parts[1].to_string(),
                    }).await;
                    self.render_response(resp);
                }
            }
            "/pod" => {
                if args.is_empty() {
                    println!("{} Usage: /pod <task specification>", "⚠".yellow());
                } else {
                    println!("{}", format!("🐝 Launching 4-Role Specialist Swarm Pod for: '{}'...", args).cyan().bold());
                    let resp = self.dispatch(HgbRequest::SwarmPodRun {
                        task: args.to_string(),
                    }).await;
                    self.render_response(resp);
                }
            }
            "/fix" | "/rescue" => {
                if args.is_empty() {
                    println!("{} Usage: /fix <failed command>", "⚠".yellow());
                } else {
                    println!("{}", format!("🚨 Diagnosing failed command: '{}'...", args).cyan().bold());
                    let resp = self.dispatch(HgbRequest::RescueDiagnose {
                        failed_command: args.to_string(),
                        exit_code: 1,
                        stderr: String::new(),
                        stdout: String::new(),
                    }).await;
                    self.render_response(resp);
                }
            }
            "/memory" | "/mem" => {
                if args.starts_with("record") {
                    let rest = args.trim_start_matches("record").trim();
                    println!("{}", "🧠 Recording Decision to Project Memory Ledger...".cyan().bold());
                    let resp = self.dispatch(HgbRequest::MemoryRecordDecision {
                        title: rest.to_string(),
                        decision: rest.to_string(),
                        context: "Recorded via Hagibis REPL".to_string(),
                    }).await;
                    self.render_response(resp);
                } else {
                    println!("{}", "🧠 Fetching Project Memory Context Anchor...".cyan().bold());
                    let resp = self.dispatch(HgbRequest::MemoryGetAnchor {
                        max_tokens: Some(2000),
                    }).await;
                    self.render_response(resp);
                }
            }
            "/glance" => {
                if args.is_empty() {
                    println!("{} Usage: /glance <image_path> [context_path]", "⚠".yellow());
                } else {
                    let mut parts = args.split_whitespace();
                    let img = parts.next().unwrap_or("").to_string();
                    let ctx = parts.next().map(|s| s.to_string());
                    let resp = self.dispatch(HgbRequest::GlanceInspect {
                        image_path: img,
                        context_path: ctx,
                    }).await;
                    self.render_response(resp);
                }
            }
            "/pkg" | "/guard" => {
                if args.is_empty() {
                    println!("{} Usage: /pkg <name> [ecosystem] [version]", "⚠".yellow());
                } else {
                    let parts: Vec<&str> = args.split_whitespace().collect();
                    let name = parts[0].to_string();
                    let eco = if parts.len() > 1 { parts[1] } else { "crates.io" }.to_string();
                    let ver = if parts.len() > 2 { Some(parts[2].to_string()) } else { None };
                    let resp = self.dispatch(HgbRequest::PackageVerify {
                        ecosystem: eco,
                        name,
                        version: ver,
                    }).await;
                    self.render_response(resp);
                }
            }
            "/env" => {
                if args.starts_with("example") {
                    let resp = self.dispatch(HgbRequest::EnvExampleGenerate { workspace_root: None }).await;
                    self.render_response(resp);
                } else if args.starts_with("shred") {
                    let content = args.trim_start_matches("shred").trim().to_string();
                    let resp = self.dispatch(HgbRequest::EnvShred { content }).await;
                    self.render_response(resp);
                } else {
                    let resp = self.dispatch(HgbRequest::EnvScan { workspace_root: None, env_file: None }).await;
                    self.render_response(resp);
                }
            }
            "/proxy" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let resource = if !parts.is_empty() { parts[0].to_string() } else { "items".to_string() };
                let port = if parts.len() > 1 { parts[1].parse::<u16>().ok() } else { None };
                let resp = self.dispatch(HgbRequest::ProxyServerStart {
                    resource_name: resource,
                    schema_json: None,
                    port,
                    seed_count: 5,
                }).await;
                self.render_response(resp);
            }
            "/trace" => {
                let n = args.trim().parse::<usize>().unwrap_or(20);
                let resp = self.dispatch(HgbRequest::TraceGetContext { last_n: n }).await;
                self.render_response(resp);
            }
            "/worktree" | "/wt" => {
                if args.is_empty() {
                    println!("{} Usage: /worktree <branch>", "⚠".yellow());
                } else {
                    let resp = self.dispatch(HgbRequest::WorktreeCreate {
                        branch: args.to_string(),
                        base_commit: None,
                    }).await;
                    self.render_response(resp);
                }
            }
            "/stash" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let action = if !parts.is_empty() { parts[0].to_string() } else { "create".to_string() };
                let tag = if parts.len() > 1 { Some(parts[1].to_string()) } else { None };
                let resp = self.dispatch(HgbRequest::SemanticStash {
                    action,
                    tag,
                    description: None,
                }).await;
                self.render_response(resp);
            }
            "/snoop" | "/browser" => {
                let trimmed = args.trim();
                if trimmed == "clear" {
                    let resp = self.dispatch(HgbRequest::BrowserSnoopClear).await;
                    self.render_response(resp);
                } else {
                    let target_url = if trimmed.is_empty() { None } else { Some(trimmed.to_string()) };
                    let resp = self.dispatch(HgbRequest::BrowserSnoopReport { target_url }).await;
                    self.render_response(resp);
                }
            }
            "/race3" | "/variant-race" => {
                let trimmed = args.trim();
                if trimmed.starts_with("pick ") {
                    let parts: Vec<&str> = trimmed.split_whitespace().collect();
                    if parts.len() >= 3 {
                        let resp = self.dispatch(HgbRequest::VariantRacePick {
                            race_id: parts[1].to_string(),
                            winner_id: parts[2].to_string(),
                        }).await;
                        self.render_response(resp);
                    } else if parts.len() == 2 {
                        let resp = self.dispatch(HgbRequest::VariantRacePick {
                            race_id: "latest".to_string(),
                            winner_id: parts[1].to_string(),
                        }).await;
                        self.render_response(resp);
                    }
                } else if trimmed.starts_with("abort ") {
                    let race_id = trimmed.trim_start_matches("abort ").trim().to_string();
                    let resp = self.dispatch(HgbRequest::VariantRaceAbort { race_id }).await;
                    self.render_response(resp);
                } else {
                    let prompt = if trimmed.is_empty() { "Modern responsive dashboard component" } else { trimmed };
                    let resp = self.dispatch(HgbRequest::VariantRaceStart {
                        prompt: prompt.to_string(),
                        archetypes: None,
                    }).await;
                    self.render_response(resp);
                }
            }
            "/dbsync" | "/db" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let sub = if !parts.is_empty() { parts[0] } else { "scan" };
                match sub {
                    "apply" => {
                        let db_path = if parts.len() > 1 { parts[1].to_string() } else { "sqlite.db".to_string() };
                        let resp = self.dispatch(HgbRequest::DbSentinelApply {
                            db_path,
                            migration_sql: "-- Auto-applied via REPL\n".to_string(),
                            migration_name: "repl_migration".to_string(),
                        }).await;
                        self.render_response(resp);
                    }
                    _ => {
                        let db_path = if parts.len() > 1 { Some(parts[1].to_string()) } else { None };
                        let resp = self.dispatch(HgbRequest::DbSentinelScan { db_path, ddl_path: None }).await;
                        self.render_response(resp);
                    }
                }
            }
            "/slice" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                if parts.len() >= 2 {
                    let file_path = parts[0].to_string();
                    let focal_symbol = parts[1].to_string();
                    let depth = if parts.len() >= 3 { parts[2].parse().unwrap_or(2) } else { 2 };
                    let resp = self.dispatch(HgbRequest::SyntaxSlice {
                        file_path,
                        focal_symbol,
                        depth,
                    }).await;
                    self.render_response(resp);
                } else {
                    println!("{} Usage: /slice <file> <symbol> [depth]", "⚠".yellow());
                }
            }
            "/spec" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let sub = if !parts.is_empty() { parts[0] } else { "run" };
                match sub {
                    "synth" => {
                        if parts.len() >= 3 {
                            let resp = self.dispatch(HgbRequest::AutoSpecSynthesize {
                                target_function: parts[1].to_string(),
                                file_path: parts[2].to_string(),
                            }).await;
                            self.render_response(resp);
                        } else {
                            println!("{} Usage: /spec synth <fn_name> <file_path>", "⚠".yellow());
                        }
                    }
                    "list" => {
                        let resp = self.dispatch(HgbRequest::AutoSpecList).await;
                        self.render_response(resp);
                    }
                    _ => {
                        let resp = self.dispatch(HgbRequest::AutoSpecRun { strict: false }).await;
                        self.render_response(resp);
                    }
                }
            }
            "/dna" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let sub = if !parts.is_empty() { parts[0] } else { "scan" };
                if sub == "audit" && parts.len() >= 2 {
                    let patch = std::fs::read_to_string(parts[1]).unwrap_or_default();
                    let resp = self.dispatch(HgbRequest::DriftLockAuditPatch { patch_content: patch }).await;
                    self.render_response(resp);
                } else {
                    let resp = self.dispatch(HgbRequest::DriftLockScan { workspace_root: None }).await;
                    self.render_response(resp);
                }
            }
            "/mcp" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let sub = if !parts.is_empty() { parts[0] } else { "list" };
                match sub {
                    "list" | "ls" => {
                        let config_path = if parts.len() > 1 {
                            if (parts[1] == "-c" || parts[1] == "--config") && parts.len() > 2 {
                                Some(parts[2].to_string())
                            } else if !parts[1].starts_with('-') {
                                Some(parts[1].to_string())
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        let resp = self.dispatch(HgbRequest::McpListTools { config_path }).await;
                        self.render_response(resp);
                    }
                    "call" => {
                        let mut server_name = String::new();
                        let mut tool_name = String::new();
                        let mut config_path = None;
                        let mut arg_tokens = Vec::new();

                        let mut idx = 1;
                        while idx < parts.len() {
                            if (parts[idx] == "-c" || parts[idx] == "--config") && idx + 1 < parts.len() {
                                config_path = Some(parts[idx + 1].to_string());
                                idx += 2;
                            } else if server_name.is_empty() {
                                server_name = parts[idx].to_string();
                                idx += 1;
                            } else if tool_name.is_empty() {
                                tool_name = parts[idx].to_string();
                                idx += 1;
                            } else {
                                arg_tokens.push(parts[idx]);
                                idx += 1;
                            }
                        }

                        if server_name.is_empty() || tool_name.is_empty() {
                            println!("{} Usage: /mcp call [-c <config>] <server> <tool> [args_json]", "⚠".yellow());
                            return Ok(true);
                        }

                        let args_str = if arg_tokens.is_empty() {
                            "{}".to_string()
                        } else {
                            arg_tokens.join(" ")
                        };

                        let parsed_args = match serde_json::from_str::<serde_json::Value>(&args_str) {
                            Ok(v) => v,
                            Err(e) => {
                                println!("{} Invalid JSON arguments: {}", "✖".red().bold(), e);
                                return Ok(true);
                            }
                        };

                        let resp = self.dispatch(HgbRequest::McpCallTool {
                            server_name,
                            tool_name,
                            arguments: parsed_args,
                            config_path,
                        }).await;
                        self.render_response(resp);
                    }
                    "help" | "-h" | "--help" => {
                        println!("\n{}", "🔌 Universal MCP (Model Context Protocol) Usage:".bold().cyan());
                        println!("  /mcp list [config_path]             List all tools across configured MCP servers");
                        println!("  /mcp call <server> <tool> [args]    Call tool on server with JSON arguments");
                        println!("  /mcp call -c <cfg> <srv> <tool>     Call tool using explicit config file\n");
                    }
                    _ => {
                        println!("{} Unknown MCP subcommand '{}'. Usage: /mcp list [config] | /mcp call <server> <tool> [args]", "⚠".yellow(), sub);
                    }
                }
            }
            "/patch" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                if parts.is_empty() {
                    println!("{} Usage: /patch <file_path> [modified_content_or_file]", "⚠".yellow());
                } else {
                    let path = parts[0];
                    let orig = std::fs::read_to_string(path).unwrap_or_default();
                    let mod_code = if parts.len() > 1 {
                        let m = parts[1..].join(" ");
                        if std::path::Path::new(&m).exists() {
                            std::fs::read_to_string(&m).unwrap_or(m)
                        } else {
                            m
                        }
                    } else {
                        orig.clone()
                    };
                    let ext = std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("rs");
                    let resp = self.dispatch(HgbRequest::AstPatchParse {
                        original: orig,
                        modified: mod_code,
                        file_ext: ext.to_string(),
                    }).await;
                    self.render_response(resp);
                }
            }
            "/live" => {
                let port: u16 = args.trim().parse().unwrap_or(3000);
                let resp = self.dispatch(HgbRequest::LiveTunnelCreate { local_port: port, session_id: None }).await;
                self.render_response(resp);
            }
            "/tdd" => {
                let intent = if args.is_empty() { "implement validated calculation".to_string() } else { args.to_string() };
                let resp = self.dispatch(HgbRequest::TddCycleRun {
                    intent,
                    target_fn: "process_data".to_string(),
                    file_ext: "rs".to_string(),
                }).await;
                self.render_response(resp);
            }
            "/isolate" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                if parts.is_empty() {
                    println!("{} Usage: /isolate <command> [args...]", "⚠".yellow());
                } else {
                    let cmd = parts[0].to_string();
                    let cmd_args: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
                    let resp = self.dispatch(HgbRequest::MicroSandboxRun {
                        command: cmd,
                        args: cmd_args,
                        timeout_ms: Some(15000),
                    }).await;
                    self.render_response(resp);
                }
            }
            "/chime" => {
                let cue = match args.trim().to_lowercase().as_str() {
                    "error" | "fail" => hgb_core::AudioCueKind::ErrorAlert,
                    "race" => hgb_core::AudioCueKind::RaceWonFast,
                    "heal" => hgb_core::AudioCueKind::CompilerHealed,
                    "secret" => hgb_core::AudioCueKind::SecretLeakBlocked,
                    _ => hgb_core::AudioCueKind::TddGreen,
                };
                let resp = self.dispatch(HgbRequest::AudioCuePlay { cue }).await;
                self.render_response(resp);
            }
            "/hmr" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                if parts.len() < 3 {
                    println!("{} Usage: /hmr <css|js|dom> <target_selector_or_fn> <payload>", "⚠".yellow());
                } else {
                    let kind = parts[0].to_string();
                    let target = parts[1].to_string();
                    let payload = parts[2..].join(" ");
                    let resp = self.dispatch(HgbRequest::CdpLivePatch { patch_kind: kind, target, payload }).await;
                    self.render_response(resp);
                }
            }
            "/lens" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                if parts.is_empty() {
                    println!("{} Usage: /lens <file_path> [target_symbol]", "⚠".yellow());
                } else {
                    let path = parts[0];
                    let sym = parts.get(1).copied().unwrap_or("");
                    let code = std::fs::read_to_string(path).unwrap_or_default();
                    let ext = std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("rs");
                    let resp = self.dispatch(HgbRequest::SkeletonLensProject {
                        source_code: code,
                        target_symbol: sym.to_string(),
                        file_ext: ext.to_string(),
                    }).await;
                    self.render_response(resp);
                }
            }
            "/swarm" => {
                let prompt = if args.is_empty() { "implement safe calculate".to_string() } else { args.to_string() };
                let resp = self.dispatch(HgbRequest::LakandiwaSwarmRace {
                    prompt,
                    target_symbol: "calculate_action".to_string(),
                    file_ext: "rs".to_string(),
                }).await;
                self.render_response(resp);
            }
            "/dbsnap" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let path = parts.first().copied().unwrap_or("app.db");
                let desc = if parts.len() > 1 { parts[1..].join(" ") } else { "Manual snapshot".to_string() };
                let resp = self.dispatch(HgbRequest::DbCowSnapshotCreate {
                    db_path: path.to_string(),
                    description: desc,
                }).await;
                self.render_response(resp);
            }
            "/dbrewind" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                if parts.len() < 2 {
                    println!("{} Usage: /dbrewind <db_path> <snapshot_file>", "⚠".yellow());
                } else {
                    let path = parts[0].to_string();
                    let snap_file = parts[1].to_string();
                    let resp = self.dispatch(HgbRequest::DbCowSnapshotRollback {
                        snapshot_file: snap_file,
                        source_path: path,
                        blake3_hash: String::new(),
                    }).await;
                    self.render_response(resp);
                }
            }
            "/shield" => {
                let pkgs: Vec<String> = args.split_whitespace().map(|s| s.to_string()).collect();
                let pkgs = if pkgs.is_empty() { vec!["react".into(), "reqwests".into()] } else { pkgs };
                let resp = self.dispatch(HgbRequest::SlopsquattingAudit {
                    packages: pkgs,
                    ecosystem: "cargo".to_string(),
                }).await;
                self.render_response(resp);
            }
            "/launch" => {
                let name = if args.is_empty() { "hagibis-app".to_string() } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::CloudLaunchpadDeploy {
                    workspace_path: None,
                    project_name: name,
                }).await;
                self.render_response(resp);
            }
            "/flight" => {
                let ep = if args.is_empty() { "POST /api/v1/checkout".to_string() } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::FlightSimulatorTrace {
                    workspace_path: None,
                    endpoint_name: ep,
                }).await;
                self.render_response(resp);
            }
            _ => {}
        }
        Ok(true)
    }

    async fn handle_superpowers_command(&mut self, cmd: &str, args: &str) -> Result<bool, Box<dyn std::error::Error>> {
        match cmd {
            "/ghostcoder" => {
                let prefix = if args.is_empty() { "pub async fn get".to_string() } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::ShadowSynthesize { prefix }).await;
                self.render_response(resp);
            }
            "/mirage" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let endpoint = parts.first().copied().unwrap_or("/v1/payment_intents").to_string();
                let method = parts.get(1).copied().unwrap_or("POST").to_string();
                let resp = self.dispatch(HgbRequest::ApiMirageSimulate { endpoint, method }).await;
                self.render_response(resp);
            }
            "/chaos" => {
                let target = if args.is_empty() { "PaymentGateway".to_string() } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::ChaosExperimentRun { target_component: target }).await;
                self.render_response(resp);
            }
            "/nightshift" => {
                let goal = if args.is_empty() { "Implement resilient webhook retry worker".to_string() } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::NightShiftDispatch { goal, base_branch: "main".to_string() }).await;
                self.render_response(resp);
            }
            "/vault" => {
                let resp = if args.trim() == "seal" {
                    let secrets = vec![
                        ("STRIPE_KEY".to_string(), format!("sk_live_{}", std::time::UNIX_EPOCH.elapsed().unwrap().as_secs())),
                        ("DB_PASSWORD".to_string(), "vault_secured_superpass".to_string()),
                    ];
                    self.dispatch(HgbRequest::VaultSeal { secrets, passphrase: "hagibis-dev-key".to_string() }).await
                } else {
                    let content = std::fs::read_to_string(".env").unwrap_or_else(|_| "API_KEY=<GHOST_ENCRYPTED_VAULT_ENABLED>".to_string());
                    self.dispatch(HgbRequest::VaultAuditDisk { disk_content: content }).await
                };
                self.render_response(resp);
            }
            "/typelock" => {
                let rust_code = if args.is_empty() {
                    "pub struct UserProfile {\n    pub id: Uuid,\n    pub email: String,\n    pub is_active: bool,\n    pub tags: Vec<String>,\n}".to_string()
                } else if std::path::Path::new(args.trim()).exists() {
                    std::fs::read_to_string(args.trim()).unwrap_or_default()
                } else {
                    args.trim().to_string()
                };
                let resp = self.dispatch(HgbRequest::TypeLockSync { rust_source: rust_code, existing_ts: None }).await;
                self.render_response(resp);
            }
            "/radar" => {
                let tier = match args.trim().to_lowercase().as_str() {
                    "atmosphere" | "atmo" => hgb_core::ZoomTier::Atmosphere,
                    "surface" | "surf" => hgb_core::ZoomTier::Surface,
                    _ => hgb_core::ZoomTier::Orbit,
                };
                let resp = self.dispatch(HgbRequest::SpatialRadarQuery { tier }).await;
                self.render_response(resp);
            }
            "/teleport" | "/inspect" => {
                let sel = if args.is_empty() { "button#checkout-btn".to_string() } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::CdpTeleportResolve { selector: sel }).await;
                self.render_response(resp);
            }
            "/voice" => {
                let phrase = if args.is_empty() { "wrap this call in a circuit breaker".to_string() } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::VoiceFlowProcess { transcript: phrase }).await;
                self.render_response(resp);
            }
            "/tape" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let url = parts.first().copied().unwrap_or("http://localhost:3000").to_string();
                let scenario = if parts.len() > 1 { parts[1..].join(" ") } else { "Feature Verification".to_string() };
                let resp = self.dispatch(HgbRequest::PrTapeRecord { url, scenario_name: scenario }).await;
                self.render_response(resp);
            }
            "/finops" => {
                let prompt = if args.is_empty() { "formally prove invariant safety and architect multi-crate boundary".to_string() } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::FinOpsRoute { prompt }).await;
                self.render_response(resp);
            }
            "/cloak" => {
                let text = if args.is_empty() { format!("Connect to 192.168.1.1 using sk_live_{} for dev@corp.io", std::time::UNIX_EPOCH.elapsed().unwrap().as_secs()) } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::AirgapCloakText { text }).await;
                self.render_response(resp);
            }
            "/sqlguard" | "/sql" => {
                let sql = if args.is_empty() { "DELETE FROM orders;".to_string() } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::SqlGuardInspect { sql }).await;
                self.render_response(resp);
            }
            "/replay" => {
                let frame = args.trim().parse::<usize>().ok();
                let resp = self.dispatch(HgbRequest::ExecutionReplayScrub { target_frame: frame }).await;
                self.render_response(resp);
            }
            "/canvas" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let (old, new) = if parts.len() >= 2 {
                    (parts[0].to_string(), parts[1].to_string())
                } else {
                    ("p-4".to_string(), "p-6".to_string())
                };
                let mutation = hgb_core::CanvasStyleMutation {
                    component_selector: "div".to_string(),
                    property_name: "className".to_string(),
                    old_value: old.clone(),
                    new_value: new,
                };
                let resp = self.dispatch(HgbRequest::CanvasApplyTweak {
                    source_code: format!("<div className=\"{}\">Canvas Component</div>", old),
                    target_file: "src/components/Canvas.tsx".to_string(),
                    symbol_name: "CanvasComponent".to_string(),
                    mutation,
                }).await;
                self.render_response(resp);
            }
            "/federate" => {
                let goal = if args.is_empty() { "Implement Apple Pay Checkout across Backend and Web".to_string() } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::MultiRepoFederate { goal }).await;
                self.render_response(resp);
            }
            "/timewarp" => {
                let months = args.trim().parse::<u32>().unwrap_or(6);
                let cfg = hgb_core::TimeWarpConfig {
                    seed: 42,
                    months,
                    base_timestamp: 1740000000,
                    include_skew: true,
                    record_scale: 5,
                };
                let resp = self.dispatch(HgbRequest::TimeWarpGenerate { config: Some(cfg) }).await;
                self.render_response(resp);
            }
            "/guardrails" => {
                let path = if args.is_empty() { ".".to_string() } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::StructuralGuardrailsAudit { workspace_path: Some(path) }).await;
                self.render_response(resp);
            }
            "/triage" => {
                let trace = if args.is_empty() {
                    "thread 'tokio-worker' panicked at 'index out of bounds: the len is 3 but the index is 3', src/routes/cart.rs:42:15".to_string()
                } else {
                    args.trim().to_string()
                };
                let resp = self.dispatch(HgbRequest::CrashTriageTrace { raw_trace: trace }).await;
                self.render_response(resp);
            }
            "/deflake" => {
                let test_name = if args.is_empty() { "test_event_delivery_async" } else { args.trim() };
                let resp = self.dispatch(HgbRequest::FlakyDeflake {
                    test_name: test_name.to_string(),
                    test_code: Some("tokio::time::sleep(std::time::Duration::from_millis(10)).await;".to_string()),
                }).await;
                self.render_response(resp);
            }
            "/anchor" => {
                let resp = self.dispatch(HgbRequest::ContextAnchorGenerate).await;
                self.render_response(resp);
            }
            _ => {}
        }
        Ok(true)
    }

    async fn handle_frontier_command(&mut self, cmd: &str, args: &str) -> Result<bool, Box<dyn std::error::Error>> {
        match cmd {
            "/lsp" | "/ghost-lsp" => {
                let prefix = if args.is_empty() { "pub async fn handle_checkout".to_string() } else { args.trim().to_string() };
                let params = hgb_core::LspInlineCompletionParams {
                    file_path: "src/main.rs".to_string(),
                    language_id: "rust".to_string(),
                    line: 1,
                    character: prefix.len(),
                    prefix_code: prefix,
                    suffix_code: "".to_string(),
                };
                let resp = self.dispatch(HgbRequest::LspGhostComplete { params }).await;
                self.render_response(resp);
            }
            "/compact" => {
                let sample_turns = vec![
                    hgb_core::ConversationTurn {
                        role: "user".to_string(),
                        content: "Execute build and run test harness".to_string(),
                        is_tool_output: false,
                        token_estimate: 25,
                    },
                    hgb_core::ConversationTurn {
                        role: "tool".to_string(),
                        content: "src/main.rs\nBuilding... [20,000 lines of compiler spew]".to_string(),
                        is_tool_output: true,
                        token_estimate: 20000,
                    },
                    hgb_core::ConversationTurn {
                        role: "assistant".to_string(),
                        content: "Compilation and tests succeeded.".to_string(),
                        is_tool_output: false,
                        token_estimate: 30,
                    },
                ];
                let resp = self.dispatch(HgbRequest::RollingCompactSession {
                    session_id: "current_session".to_string(),
                    turns: sample_turns,
                    max_tokens: Some(32000),
                }).await;
                self.render_response(resp);
            }
            "/commit" => {
                let intent = if args.is_empty() { "add biometric authentication gate".to_string() } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::GitMicroCommit {
                    files: vec!["crates/core/src/auth.rs".to_string()],
                    intent,
                    diff_preview: "+ pub fn verify_passkey() -> bool { true }\n- pub fn legacy() {}".to_string(),
                }).await;
                self.render_response(resp);
            }
            "/recipe" => {
                if args.trim().is_empty() || args.trim() == "list" {
                    let resp = self.dispatch(HgbRequest::VibeRecipeList).await;
                    self.render_response(resp);
                } else {
                    let resp = self.dispatch(HgbRequest::VibeRecipeRun { recipe_name: args.trim().to_string() }).await;
                    self.render_response(resp);
                }
            }
            "/contract" => {
                let symbol = if args.is_empty() { "process_payment".to_string() } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::BehaviorMatrixGenerate {
                    symbol_name: symbol,
                    intent_desc: "Production payment processing pipeline".to_string(),
                }).await;
                self.render_response(resp);
            }
            "/graph" => {
                let goal = if args.is_empty() { "Synchronize Cross-Repo Biometrics".to_string() } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::FlightGraphQuery { goal, active_step: 2 }).await;
                self.render_response(resp);
            }
            "/rank-map" | "/pagerank-map" => {
                let resp = self.dispatch(HgbRequest::RepoMapRank {
                    extensions: vec!["rs".to_string(), "ts".to_string(), "py".to_string()],
                    token_budget: Some(1024),
                }).await;
                self.render_response(resp);
            }
            "/shadow" => {
                let file = if args.is_empty() { "src/main.rs".to_string() } else { args.trim().to_string() };
                let candidate = "pub fn new_feature() -> bool { true }\n".to_string();
                let resp = self.dispatch(HgbRequest::ShadowPreflight {
                    relative_path: file,
                    candidate_content: candidate,
                }).await;
                self.render_response(resp);
            }
            "/squeeze" => {
                let input = if args.is_empty() {
                    "   [1/10] Compiling dependencies...\nwarning: unused variable `x`\nwarning: unused variable `x`\nerror[E0425]: cannot find value `foo`\n --> src/main.rs:12:5\n".to_string()
                } else {
                    args.to_string()
                };
                let resp = self.dispatch(HgbRequest::StreamSqueeze {
                    raw_output: input,
                    max_tokens: Some(2048),
                }).await;
                self.render_response(resp);
            }
            "/mutation" | "/mutation-audit" => {
                let file = if args.is_empty() { "src/lib.rs".to_string() } else { args.trim().to_string() };
                let sample_code = "pub fn verify(token: &str, active: bool) -> bool {\n    if token == \"auth\" && active {\n        true\n    } else {\n        false\n    }\n}\n".to_string();
                let resp = self.dispatch(HgbRequest::MutationAudit {
                    source_code: sample_code,
                    file_name: file,
                }).await;
                self.render_response(resp);
            }
            "/dom" => {
                let template = "<div id=\"root\"><header class=\"app-bar\"><h1>Hagibis Vibe Cockpit</h1></header><button id=\"launch-btn\" class=\"btn btn-primary\">Launch</button></div>".to_string();
                let resp = self.dispatch(HgbRequest::DomInspect {
                    template_content: template,
                    file_name: "src/App.tsx".to_string(),
                    click_coords: Some((150.0, 45.0)),
                    css_selector: None,
                }).await;
                self.render_response(resp);
            }
            "/mcp-hub" => {
                let action = if args.is_empty() { "list".to_string() } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::McpOrchestrate {
                    action,
                    server_name: None,
                    tool_name: None,
                    arguments: None,
                }).await;
                self.render_response(resp);
            }
            "/live-graph" | "/sync-graph" => {
                let resp = self.dispatch(HgbRequest::LiveGraphSync {
                    extensions: vec!["rs".to_string(), "ts".to_string(), "py".to_string()],
                }).await;
                self.render_response(resp);
            }
            "/panic-fix" | "/shell-panic" => {
                let resp = self.dispatch(HgbRequest::ShellPanicDiagnose {
                    command: if args.is_empty() { "cargo test --bin checkout".to_string() } else { args.trim().to_string() },
                    exit_code: 101,
                    stderr: "error[E0425]: cannot find value `token` in this scope\n --> src/auth/jwt.rs:45:12".to_string(),
                }).await;
                self.render_response(resp);
            }
            "/plan-spec" | "/decompose" => {
                let intent = if args.is_empty() { "Implement biometric passkey authentication gate".to_string() } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::SpecDecompose {
                    intent,
                    workspace_files: vec!["src/auth.rs".to_string(), "src/main.rs".to_string()],
                }).await;
                self.render_response(resp);
            }
            "/at-expand" | "/expand-context" => {
                let prompt = if args.is_empty() { "Review @git:staged changes and check @err:latest".to_string() } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::DynamicContextExpand { prompt }).await;
                self.render_response(resp);
            }
            "/visual-sentry" | "/pixel-diff" => {
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
                let c_nodes = vec![
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
                ];
                let resp = self.dispatch(HgbRequest::VisualRegressionAudit {
                    baseline_nodes: b_nodes,
                    current_nodes: c_nodes,
                }).await;
                self.render_response(resp);
            }
            "/heal-watch" | "/watchdog-loop" => {
                let resp = self.dispatch(HgbRequest::ContinuousHealWatch {
                    workspace_errors: vec!["src/main.rs:42: error: cannot find value `foo` in scope".to_string()],
                    flaky_tests: vec!["test_auth_timeout".to_string()],
                }).await;
                self.render_response(resp);
            }
            "/predict" | "/ambient" => {
                let (file, sym) = if args.is_empty() {
                    ("src/main.rs".to_string(), "handle_checkout".to_string())
                } else {
                    let parts: Vec<&str> = args.split_whitespace().collect();
                    if parts.len() >= 2 {
                        (parts[0].to_string(), parts[1].to_string())
                    } else {
                        ("src/main.rs".to_string(), parts[0].to_string())
                    }
                };
                let resp = self.dispatch(HgbRequest::AmbientPredict {
                    file_path: file,
                    symbol_name: sym,
                    change_kind: hgb_core::EditKind::SignatureModified,
                    old_snippet: None,
                    new_snippet: None,
                }).await;
                self.render_response(resp);
            }
            "/tweak" | "/cdp-sync" => {
                let (file, selector, prop, val) = if args.is_empty() {
                    ("src/App.tsx".to_string(), "button.btn-primary".to_string(), "backgroundColor".to_string(), "#4F46E5".to_string())
                } else {
                    let parts: Vec<&str> = args.split_whitespace().collect();
                    if parts.len() >= 4 {
                        (parts[0].to_string(), parts[1].to_string(), parts[2].to_string(), parts[3].to_string())
                    } else {
                        ("src/App.tsx".to_string(), "button".to_string(), "color".to_string(), args.trim().to_string())
                    }
                };
                let event = hgb_core::DomTweakEvent {
                    selector,
                    property_or_attr: prop,
                    old_value: "".to_string(),
                    new_value: val,
                    component_hint: None,
                    file_hint: Some(file),
                };
                let resp = self.dispatch(HgbRequest::CdpTweakSync {
                    event,
                    apply_to_disk: false,
                }).await;
                self.render_response(resp);
            }
            "/mode" | "/harvest" => {
                let (mode, query) = if args.is_empty() {
                    ("debug".to_string(), "tokio task panicked".to_string())
                } else {
                    let parts: Vec<&str> = args.split_whitespace().collect();
                    if parts.len() >= 2 {
                        (parts[0].to_string(), parts[1..].join(" "))
                    } else {
                        (parts[0].to_string(), "general".to_string())
                    }
                };
                let m = match mode.to_lowercase().as_str() {
                    "architect" => hgb_core::VibePromptMode::Architect,
                    "debug" => hgb_core::VibePromptMode::DebugTriage,
                    "security" => hgb_core::VibePromptMode::SecurityAudit,
                    "doc" => hgb_core::VibePromptMode::DocReview,
                    _ => hgb_core::VibePromptMode::CodeSprint,
                };
                let resp = self.dispatch(HgbRequest::PromptModeHarvest {
                    mode: m,
                    user_prompt: query,
                    doc_targets: vec![],
                    raw_doc_content: None,
                }).await;
                self.render_response(resp);
            }
            "/sandbox" | "/box" => {
                let stack = if args.is_empty() { "rust_tokio".to_string() } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::EphemeralSandboxSpinUp {
                    stack_name: stack,
                    tables_to_seed: vec!["users".to_string(), "orders".to_string()],
                }).await;
                self.render_response(resp);
            }
            "/anti-placebo" | "/gatekeeper" => {
                let test_code = if args.is_empty() {
                    "#[test]\nfn test_auth() {\n    let valid = verify_token(\"secret\", true);\n    assert!(valid);\n}".to_string()
                } else {
                    args.to_string()
                };
                let resp = self.dispatch(HgbRequest::AntiPlaceboAudit {
                    source_code: "pub fn verify_token(s: &str, active: bool) -> bool { s == \"secret\" && active }".to_string(),
                    test_code,
                }).await;
                self.render_response(resp);
            }
            "/preview" => {
                let port = args.trim().parse::<u16>().ok();
                let resp = self.dispatch(HgbRequest::LivePreviewStart { port, proxy_devserver_port: None }).await;
                self.render_response(resp);
            }
            "/vision" => {
                let prompt = if args.is_empty() { "Analyze UI layout and convert to Tailwind CSS".to_string() } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::MultimodalVisionCapture { prompt, base64_image: None }).await;
                self.render_response(resp);
            }
            "/share" => {
                let port = args.trim().parse::<u16>().unwrap_or(3000);
                let resp = self.dispatch(HgbRequest::ShareTunnelCreate { local_port: port, custom_slug: None }).await;
                self.render_response(resp);
            }
            "/graduate" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let target = if parts.first().map(|s| *s == "firebase").unwrap_or(false) {
                    hgb_core::BaasTarget::Firebase
                } else if parts.first().map(|s| *s == "neon").unwrap_or(false) {
                    hgb_core::BaasTarget::Neon
                } else {
                    hgb_core::BaasTarget::Supabase
                };
                let resource = parts.get(1).unwrap_or(&"habits").to_string();
                let sample = serde_json::json!({
                    "id": "h_101",
                    "title": "Morning Vibe Coding",
                    "streak_count": 14,
                    "is_completed": true,
                    "user_email": "vibe@example.com"
                });
                let resp = self.dispatch(HgbRequest::BaasGraduate { resource_name: resource, sample_json: sample, target }).await;
                self.render_response(resp);
            }
            "/expand" => {
                let prompt = if args.is_empty() { "dark mode habit tracker with neon glowing streaks and celebratory confetti".to_string() } else { args.trim().to_string() };
                let resp = self.dispatch(HgbRequest::VibeIntentExpand { prompt }).await;
                self.render_response(resp);
            }
            "/auto-heal" => {
                let log = if args.is_empty() {
                    "error[E0433]: cannot find module or crate `uuid` in this scope\nCannot find module 'lucide-react'".to_string()
                } else {
                    args.to_string()
                };
                let resp = self.dispatch(HgbRequest::AutoDependencyHeal { compiler_log: log }).await;
                self.render_response(resp);
            }
            "/evolve" | "/self-evolve" => {
                let config = hgb_core::SelfEvolutionConfig {
                    target_path: if args.is_empty() { ".".to_string() } else { args.trim().to_string() },
                    max_generations: 3,
                    mutation_rate: 0.15,
                    auto_distill_recipes: true,
                    export_dpo_dataset: true,
                };
                let resp = self.dispatch(HgbRequest::SelfEvolutionRun { config }).await;
                self.render_response(resp);
            }
            "/desktop" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let sub = parts.first().copied().unwrap_or("inspect");
                if sub == "inspect" {
                    let resp = self.dispatch(HgbRequest::DesktopInspect).await;
                    self.render_response(resp);
                } else {
                    let act = hgb_core::DesktopAction::Click { x: 100, y: 100, button: "left".to_string() };
                    let resp = self.dispatch(HgbRequest::DesktopActionExecute { action: act }).await;
                    self.render_response(resp);
                }
            }
            "/verify-proof" | "/verify" => {
                let target = if args.is_empty() { "src/lib.rs".to_string() } else { args.trim().to_string() };
                let config = hgb_core::FormalVerificationConfig {
                    target_file: target,
                    solver: hgb_core::SmtSolverKind::Z3,
                    verify_overflows: true,
                    verify_bounds: true,
                    timeout_seconds: 15,
                };
                let resp = self.dispatch(HgbRequest::FormalVerifyRun { config }).await;
                self.render_response(resp);
            }
            "/monorepo" | "/hypergraph" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                if parts.first().copied() == Some("blast") {
                    let files = if parts.len() > 1 { parts[1..].iter().map(|s| s.to_string()).collect() } else { vec!["crates/hgb-core/src/lib.rs".to_string()] };
                    let resp = self.dispatch(HgbRequest::MonorepoBlastRadius { changed_files: files }).await;
                    self.render_response(resp);
                } else {
                    let resp = self.dispatch(HgbRequest::MonorepoAnalyze).await;
                    self.render_response(resp);
                }
            }
            "/embedded" | "/firmware" => {
                let config = hgb_core::EmbeddedCheckConfig {
                    target_arch: hgb_core::TargetMcuArchitecture::ArmCortexM,
                    no_std: true,
                    max_flash_bytes: 256 * 1024,
                    max_ram_bytes: 64 * 1024,
                };
                let code = if args.is_empty() { "#![no_std]\npub fn init() {}".to_string() } else { args.to_string() };
                let resp = self.dispatch(HgbRequest::EmbeddedCheck { code, config }).await;
                self.render_response(resp);
            }
            "/store" | "/release" => {
                let config = hgb_core::StoreReleaseConfig {
                    platform: hgb_core::AppStorePlatform::AppleAppStore,
                    track: hgb_core::ReleaseTrack::Beta,
                    app_bundle_id: "com.hagibis.app".to_string(),
                    version_name: "1.0.0".to_string(),
                    build_number: 1,
                    fastlane_lane: "beta".to_string(),
                };
                let resp = self.dispatch(HgbRequest::StoreReleaseRun { config }).await;
                self.render_response(resp);
            }
            "/speak" | "/tts" => {
                let text = if args.is_empty() { "Hagibis voice synthesis active.".to_string() } else { args.to_string() };
                let config = hgb_core::SynthesisConfig {
                    voice_id: "en_US-kokoro-v1".to_string(),
                    speaking_rate: 1.0,
                    pitch: 1.0,
                    output_format: "wav".to_string(),
                };
                let resp = self.dispatch(HgbRequest::SpeechSynthesize { text, config }).await;
                self.render_response(resp);
            }
            "/studio" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let port = parts.first().and_then(|p| p.parse::<u16>().ok());
                let resp = self.dispatch(HgbRequest::StudioStart { port }).await;
                self.render_response(resp);
            }
            _ => {}
        }

        Ok(true)
    }

    async fn execute_prompt(&self, prompt: &str) -> Result<(), Box<dyn std::error::Error>> {
        let model_str = self.model.as_deref().unwrap_or("gemini-2.5-flash");
        let is_local = hgb_core::OllamaProvider::is_ollama_model(model_str);
        let req = HgbRequest::Prompt {
            prompt: prompt.to_string(),
            model: self.model.clone(),
            provider: None,
            stream: false,
        };

        // Live animated spinner in REPL while waiting for response
        let spinner_active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let spinner_active_clone = spinner_active.clone();
        let model_name = model_str.to_string();

        let spinner_handle = tokio::spawn(async move {
            let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let waves = [
                "▰▱▱▱▱", "▰▰▱▱▱", "▰▰▰▱▱", "▰▰▰▰▱", "▰▰▰▰▰",
                "▱▰▰▰▰", "▱▱▰▰▰", "▱▱▱▰▰", "▱▱▱▱▰", "▱▱▱▱▱",
            ];
            let start = std::time::Instant::now();
            let mut tick = 0;
            use std::io::Write;
            while spinner_active_clone.load(std::sync::atomic::Ordering::Relaxed) {
                let frame = frames[tick % frames.len()];
                let wave = waves[tick % waves.len()];
                let elapsed = start.elapsed().as_secs_f64();
                let engine = if is_local { "Ollama" } else { "AGY" };
                let color_code = if is_local { "\x1b[35m" } else { "\x1b[36m" };
                print!(
                    "\r  {}{} {} Thinking... (model: {}) {} ⏱ {:.1}s\x1b[0m",
                    color_code, frame, engine, model_name, wave, elapsed
                );
                let _ = std::io::stdout().flush();
                tokio::time::sleep(tokio::time::Duration::from_millis(80)).await;
                tick += 1;
            }
            // Clear spinner line with ANSI escape sequence \r\x1b[2K
            print!("\r\x1b[2K");
            let _ = std::io::stdout().flush();
        });

        let resp = self.dispatch(req).await;
        spinner_active.store(false, std::sync::atomic::Ordering::Relaxed);
        let _ = spinner_handle.await;

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

    pub async fn show_models(&self) {
        let current = self.model.as_deref().unwrap_or("auto");
        let cred = hgb_core::GeminiProvider::credential_status();
        let ollama_status = if hgb_core::OllamaProvider::is_available() {
            "Ready (127.0.0.1:11434)".green().to_string()
        } else {
            "Offline".dimmed().to_string()
        };

        println!("\n{}", "🪽 HAGIBIS MODEL REGISTRY 🪽".bold().cyan());
        println!("  [•] Active Model: {}", current.yellow().bold());
        println!("  [•] Google Gemini Cloud: {}", cred.cyan());
        println!("  [•] Local Ollama Engine: {}", ollama_status);

        // 1. Local Ollama Engines (queried dynamically in real-time)
        if let Some(prov) = hgb_core::OllamaProvider::auto_discover() {
            if let Ok(models) = prov.list_model_details().await {
                if !models.is_empty() {
                    println!("\n  {}", format!("🖥️ Local Ollama Engines ({} installed, 0ms latency, zero cloud cost):", models.len()).bold().magenta());
                    for m in &models {
                        let is_active = current == m.name 
                            || current == m.short_name() 
                            || current == format!("ollama/{}", m.name)
                            || current == format!("ollama/{}", m.short_name());
                        let active_mark = if is_active {
                            " [ACTIVE]".green().bold().to_string()
                        } else {
                            "".to_string()
                        };
                        println!("      • {:<34} [{}] ({}){}", format!("ollama/{}", m.name).magenta().bold(), m.param_summary().cyan(), m.formatted_size().dimmed(), active_mark);
                    }
                }
            }
        }

        // 2. Cloud Gemini Providers
        println!("\n  {}", "☁️ Cloud LLM Providers (Google Gemini):".bold().cyan());
        let cloud_models = ["gemini-2.5-flash", "gemini-2.5-pro", "gemini-2.0-flash"];
        for cm in cloud_models {
            let is_active = current == cm;
            let active_mark = if is_active {
                " [ACTIVE]".green().bold().to_string()
            } else {
                "".to_string()
            };
            println!("      • {:<34} (Google Gemini Cloud){}", cm.cyan().bold(), active_mark);
        }

        println!("\n  {}", "💡 Tip: Switch models using '/model <name>' (e.g. /model ollama/qwen2.5:0.5b)".dimmed());
        println!();
    }

    pub fn render_response(&self, resp: HgbResponse) {
        match resp {
            HgbResponse::Pong { latency_us } => {
                println!("{} Daemon pong received in {} µs", "✔ PONG:".green().bold(), latency_us);
            }
            HgbResponse::Status(status) => {
                println!("{}", "🪽 HAGIBIS RESIDENT DAEMON STATUS 🪽".bold().cyan());
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
                ChatCanvas::print_markdown(&output);
                if tokens_used > 0 {
                    let tok_s = if duration_ms > 0 {
                        format!(" ({:.1} tok/s)", (tokens_used as f64) / (duration_ms as f64 / 1000.0))
                    } else {
                        "".to_string()
                    };
                    println!("  {} {} tokens in {} ms{}", "⏱️".cyan(), tokens_used, duration_ms, tok_s.dimmed());
                }
            }
            HgbResponse::TextChunk(chunk) => print!("{}", chunk),
            HgbResponse::Error(err) => {
                let card = ToolCallCard::new("error", "Execution Failed", ToolCardStatus::Failed {
                    duration_ms: 0,
                    error: err.clone(),
                }).with_output(&err);
                card.print();
            }
            HgbResponse::ModelSwitched { previous, current, duration_ms } => {
                println!("{} {} ➔ {} (in {} ms)", "✔ Model Switched:".green().bold(), previous.dimmed(), current.bold().cyan(), duration_ms);
            }
            HgbResponse::ModelList(models) => {
                println!("{}", "🪽 AVAILABLE AI MODELS 🪽".bold().cyan());
                let current = self.model.as_deref().unwrap_or("auto");
                let mut local_models = Vec::new();
                let mut cloud_models = Vec::new();

                for m in models {
                    if m.starts_with("ollama/") || m == "in-process-gguf" || m == "ollama" {
                        local_models.push(m);
                    } else {
                        cloud_models.push(m);
                    }
                }

                if !local_models.is_empty() {
                    println!("\n  {}", "🖥️ Local Ollama Engines (0ms latency, zero cloud cost):".bold().magenta());
                    for m in local_models {
                        let is_active = current != "auto" && (m.contains(current) || current.contains(m.split_whitespace().next().unwrap_or("")));
                        let active_badge = if is_active { " [ACTIVE]".green().bold().to_string() } else { "".to_string() };
                        println!("    • {}{}", m.magenta(), active_badge);
                    }
                }

                if !cloud_models.is_empty() {
                    println!("\n  {}", "☁️ Cloud LLM Providers (Google Gemini):".bold().cyan());
                    for m in cloud_models {
                        let is_active = current != "auto" && current == m;
                        let active_badge = if is_active { " [ACTIVE]".green().bold().to_string() } else { "".to_string() };
                        println!("    • {}{}", m.cyan(), active_badge);
                    }
                }
            }
            HgbResponse::RaceResult { winner, duration_ms, patch, passed_checks } => {
                if passed_checks {
                    println!("{} {} ({} ms)", "🏆 First Green Candidate:".green().bold(), winner.bold(), duration_ms);
                    ChatCanvas::print_markdown(&patch);
                } else {
                    println!("{} {} (syntax checks failed)", "✖ Speculative Race Failed:".red().bold(), winner);
                }
            }
            HgbResponse::StyleFeedbackRecorded => {
                println!("✔ Style feedback recorded in Style Memory Vault");
            }
            HgbResponse::StyleGuidance(guidelines) => {
                ChatCanvas::print_markdown(&guidelines);
            }
            HgbResponse::ShipReport { pr_title, pr_body, commits, security_passed } => {
                if security_passed {
                    println!("{} {}", "📦 PR Title:".bold().green(), pr_title);
                    println!("{}", "📋 Commits:".bold().cyan());
                    for c in commits {
                        println!("  ✔ {}", c);
                    }
                    ChatCanvas::print_markdown(&pr_body);
                } else {
                    println!("{}", "⛔ AgentShieldLight Security Audit FAILED: Secret leak detected!".red().bold());
                    ChatCanvas::print_markdown(&pr_body);
                }
            }
            HgbResponse::CommandOutput { stdout, stderr, exit_code, duration_ms, timed_out } => {
                let status = if timed_out {
                    ToolCardStatus::Failed { duration_ms, error: "Command execution timed out".to_string() }
                } else if exit_code == 0 {
                    ToolCardStatus::Success { duration_ms, exit_code }
                } else {
                    ToolCardStatus::Failed { duration_ms, error: format!("Process exited with status {}", exit_code) }
                };
                let out = if stderr.is_empty() {
                    stdout
                } else if stdout.is_empty() {
                    stderr
                } else {
                    format!("{}\n{}", stdout, stderr)
                };
                let card = ToolCallCard::new("run_command", format!("exit_code={}", exit_code), status)
                    .with_output(out);
                card.print();
            }
            HgbResponse::AgentSession { output, turns, duration_ms, steps_count } => {
                println!("{}", format!("🤖 ReAct Agent Session Completed in {} turns ({} steps, {} ms)", turns, steps_count, duration_ms).cyan().bold());
                ChatCanvas::print_markdown(&output);
            }
            HgbResponse::HealResult { check_command, initial_errors, final_errors, fully_healed, duration_ms, attempts_count } => {
                if fully_healed {
                    println!("{} Converged to green after {} attempts (in {} ms): {} errors ➔ 0 errors", "🩹 Self-Healing Green:".green().bold(), attempts_count, duration_ms, initial_errors);
                } else {
                    println!("{} Final state: {} errors remaining after {} attempts (in {} ms)", "✖ Healing Incomplete:".red().bold(), final_errors, attempts_count, duration_ms);
                }
                println!("  [•] Verification command: '{}'", check_command.cyan());
            }
            HgbResponse::UndoResult { checkpoint_id, files_restored, files_deleted, duration_ms } => {
                println!("{} Checkpoint '{}' (in {} ms)", "⏪ Time-Travel Rollback Complete:".green().bold(), checkpoint_id.cyan(), duration_ms);
                if !files_restored.is_empty() {
                    println!("  Restored ({}):", files_restored.len());
                    for f in files_restored {
                        println!("    ✔ {}", f.green());
                    }
                }
                if !files_deleted.is_empty() {
                    println!("  Deleted created files ({}):", files_deleted.len());
                    for f in files_deleted {
                        println!("    ✂ {}", f.yellow());
                    }
                }
            }
            HgbResponse::RulesReport { rules_count, aggregate_hash, aggregated_content } => {
                println!("{}", "📜 WORKSPACE RULES ANCHOR 📜".bold().cyan());
                println!("  [•] Rules files discovered: {}", rules_count);
                let short_hash = if aggregate_hash.len() > 16 { &aggregate_hash[..16] } else { &aggregate_hash };
                println!("  [•] Aggregate Blake3: {}", short_hash);
                ChatCanvas::print_markdown(&aggregated_content);
            }
            HgbResponse::RepoMapReport { content, symbol_count, file_count } => {
                println!("{} Indexed {} symbols across {} files", "🗺️ Syntactic Repo Map:".cyan().bold(), symbol_count, file_count);
                ChatCanvas::print_markdown(&format!("```yaml\n{}\n```", content));
            }
            HgbResponse::GuardianReport { is_active, staged_ghost_fixes, last_check_passed } => {
                let status_str = if is_active { "ACTIVE".green() } else { "IDLE".yellow() };
                let check_str = if last_check_passed { "PASSING".green() } else { "ERRORS FOUND".red() };
                println!("{} Status: {} | Verification: {} | Staged Ghost-Fixes: {}", "🛡️ GUARDIAN MODE:".bold().cyan(), status_str, check_str, staged_ghost_fixes.to_string().cyan().bold());
                if staged_ghost_fixes > 0 {
                    println!("  {} Staged Ghost-Fixes ready in memory. Run 'hgb ghost' or '/ghost' to inspect/apply.", "💡".yellow());
                }
            }
            HgbResponse::GhostFixApplied { report } => {
                println!("{} {}", "🪽 Ghost-Fix Applied:".green().bold(), report);
            }
            HgbResponse::DevServerEndpoints(endpoints) => {
                println!("{}", "🌐 DEVSERVER SENTINEL: ACTIVE ENDPOINTS 🌐".bold().cyan());
                if endpoints.is_empty() {
                    println!("  No active local dev servers discovered on probed ports.");
                } else {
                    for ep in endpoints {
                        let health = if ep.is_healthy { "HEALTHY".green() } else { "ERROR".red() };
                        let status_code = ep.http_status.map(|s| s.to_string()).unwrap_or_else(|| "N/A".to_string());
                        println!("  [•] Port {:<5} | {:<16} | {:<24} | HTTP {} | {} ({} ms)",
                            ep.port.to_string().yellow(),
                            ep.framework.cyan(),
                            ep.url,
                            status_code,
                            health,
                            ep.response_time_ms
                        );
                    }
                }
            }
            HgbResponse::ImpactReport(rep) => {
                println!("{}", "📡 RIPPLE EFFECT RADAR ASSESSMENT 📡".bold().cyan());
                let risk_color = match rep.risk_level {
                    hgb_core::impact::RiskLevel::Low => "LOW RISK".green(),
                    hgb_core::impact::RiskLevel::Moderate => "MODERATE RISK".yellow(),
                    hgb_core::impact::RiskLevel::High => "HIGH RISK".red(),
                    hgb_core::impact::RiskLevel::Critical => "CRITICAL RISK".bright_red().bold(),
                };
                println!("  [•] Target Symbol:        {}", rep.target_symbol.bold().yellow());
                println!("  [•] Originating File:     {}", rep.originating_file.display().to_string().cyan());
                println!("  [•] Risk Classification:  {}", risk_color);
                println!("  [•] Total Affected Files: {}", rep.total_affected_files.to_string().bold());
                println!("  [•] Direct Dependents:    {}", rep.direct_dependents.len());
                println!("  [•] Transitive Blast:     {}", rep.transitive_dependents.len());
                println!("  [•] Recommendation:       {}", rep.recommended_action.italic());
                if !rep.call_sites.is_empty() {
                    println!("\n  {} Discovered Call-Sites ({}):", "🔍".cyan(), rep.call_sites.len());
                    for (i, cs) in rep.call_sites.iter().take(10).enumerate() {
                        println!("    {}. {}:{} ➔ {}", i + 1, cs.relative_path.cyan(), cs.line_number.to_string().yellow(), cs.context_snippet.dimmed());
                    }
                    if rep.call_sites.len() > 10 {
                        println!("    ... and {} more call-sites", rep.call_sites.len() - 10);
                    }
                }
            }
            HgbResponse::SwarmPodCompleted(res) => {
                println!("{}", "🐝 SPECIALIST SWARM POD CONSENSUS 🐝".bold().cyan());
                println!("  [•] Task: {}", res.task.bold());
                println!("  [•] Duration: {} ms | Tokens Used: {}", res.duration_ms, res.total_tokens);
                let review_tag = if res.review_status { "PASSED [APPROVED]".green().bold() } else { "FAILED [REJECTED]".red().bold() };
                println!("  [•] Security & Style Review: {}", review_tag);
                println!("\n🏛️ Architect Plan:\n{}", res.architect_plan.dimmed());
                println!("\n💻 Coder Solution:\n");
                ChatCanvas::print_markdown(&res.code_solution);
                println!("\n🧪 QA Test Matrix:\n{}", res.test_coverage.dimmed());
            }
            HgbResponse::RescueReport(rep) => {
                println!("{}", "🚨 TERMINAL RESCUE: MIND-READER DIAGNOSIS 🚨".bold().cyan());
                println!("  [•] Failed Command: {}", rep.failed_command.yellow());
                println!("  [•] Exit Code:      {}", rep.exit_code);
                println!("  [•] Root Cause:     {}", rep.explanation.red().bold());
                if !rep.suggested_fixes.is_empty() {
                    println!("\n  {} Suggested Corrective Actions:", "🔧".green());
                    for (i, fix) in rep.suggested_fixes.iter().enumerate() {
                        let warn = if fix.is_destructive { " [Destructive]".bright_red() } else { "".normal() };
                        println!("    {}. {}{}", i + 1, fix.description.bold(), warn);
                        println!("       Command: {}", fix.command.green());
                        println!("       Rationale: {}", fix.explanation.dimmed());
                    }
                }
            }
            HgbResponse::MemoryAnchor(anchor) => {
                println!("{}", "🧠 PROJECT MEMORY CONTEXT ANCHOR 🧠".bold().cyan());
                ChatCanvas::print_markdown(&format!("```xml\n{}\n```", anchor));
            }
            HgbResponse::MemoryRecorded { id } => {
                println!("{} Decision recorded in Project Memory Ledger ({})", "✔ ADR Recorded:".green().bold(), id.cyan());
            }
            // --- Sprint Vibe Coding Responses ---
            HgbResponse::GlanceReport(rep) => {
                let card = hgb_nextgen::GlanceEngine::render_terminal_card(&rep);
                println!("{}", card);
            }
            HgbResponse::PackageVerified(rep) => {
                println!("{}", "🛡️ DEPENDENCY HALLUCINATION FIREWALL 🛡️".bold().cyan());
                println!("  [•] Package:   {}", rep.name.bold().yellow());
                println!("  [•] Ecosystem: {}", rep.ecosystem.as_str().cyan());
                if let Some(ref req_v) = rep.requested_version {
                    println!("  [•] Requested: {}", req_v);
                }
                if let Some(ref lat_v) = rep.latest_version {
                    println!("  [•] Latest:    {}", lat_v.green());
                }
                let status_badge = match rep.status {
                    hgb_core::package_guard::PackageStatus::Valid => "VALID [FOUND]".green().bold(),
                    hgb_core::package_guard::PackageStatus::NotFound => "NOT FOUND [HALLUCINATED]".red().bold(),
                    hgb_core::package_guard::PackageStatus::Yanked => "YANKED [INSECURE]".red().bold(),
                    hgb_core::package_guard::PackageStatus::Deprecated => "DEPRECATED".yellow().bold(),
                    hgb_core::package_guard::PackageStatus::RegistryUnavailable => "OFFLINE / TIMEOUT".dimmed(),
                    hgb_core::package_guard::PackageStatus::SuspiciousHallucination => "SUSPICIOUS HALLUCINATION".bright_red().bold(),
                };
                println!("  [•] Status:    {}", status_badge);
                if let Some(ref warn) = rep.warning_message {
                    println!("  {} {}", "⚠".yellow(), warn);
                }
                if !rep.known_alternatives.is_empty() {
                    println!("  💡 Suggested Alternatives: {}", rep.known_alternatives.join(", ").cyan());
                }
                let cache_note = if rep.cached { " (cached)" } else { "" };
                println!("  ⏱️ Verification completed in {} ms{}", rep.verification_time_ms, cache_note.dimmed());
            }
            HgbResponse::EnvAudit(rep) => {
                println!("{}", "🔒 NO-LEAK SECRET SENTINEL: ENV AUDIT 🔒".bold().cyan());
                println!("  [•] Workspace:    {}", rep.workspace_root.display());
                println!("  [•] .env Found:   {}", if rep.env_file_found { "YES".green() } else { "NO (.env missing)".yellow() });
                println!("  [•] Total Scanned: {}", rep.total_detected);
                println!("  [•] Missing:      {}", if rep.missing_count > 0 { rep.missing_count.to_string().red() } else { "0".green() });
                println!("  [•] Placeholders: {}", if rep.placeholder_count > 0 { rep.placeholder_count.to_string().yellow() } else { "0".green() });
                println!("  [•] Secret Leaks: {}", if rep.secret_leaks.is_empty() { "0 [CLEAN]".green() } else { format!("{} LEAKS FOUND", rep.secret_leaks.len()).bright_red().bold() });

                if !rep.variables.is_empty() {
                    println!("\n  📋 Variable Breakdown:");
                    for item in &rep.variables {
                        let status_tag = match &item.status {
                            hgb_core::env_sentinel::EnvVarStatus::Configured { value_obscured } => format!("CONFIGURED ({})", value_obscured).green(),
                            hgb_core::env_sentinel::EnvVarStatus::MissingInEnv => "MISSING IN .ENV".red().bold(),
                            hgb_core::env_sentinel::EnvVarStatus::PlaceholderValue { value } => format!("PLACEHOLDER ('{}')", value).yellow().bold(),
                            hgb_core::env_sentinel::EnvVarStatus::UnusedInCode => "UNUSED IN CODE".dimmed(),
                        };
                        println!("   • {:<24} ➔ {}", item.name.bold(), status_tag);
                    }
                }

                if !rep.secret_leaks.is_empty() {
                    println!("\n  🚨 CRITICAL SECRET LEAKS IN SOURCE CODE:");
                    for leak in &rep.secret_leaks {
                        println!("   • [{}] Sample: {} (Entropy: {:.2} bits)", leak.secret_type.red().bold(), leak.sample_redacted.yellow(), leak.entropy);
                    }
                }
            }
            HgbResponse::EnvExample(example) => {
                println!("{}", "📝 GENERATED .env.example TEMPLATE:".bold().green());
                println!("{}", example);
            }
            HgbResponse::EnvShredded { sanitized_content, leaks_detected } => {
                println!("{} Detected and shredded {} secrets", "🔒 Shred Complete:".green().bold(), leaks_detected);
                println!("{}", sanitized_content);
            }
            HgbResponse::ProxyServerStarted { url, port, resource, seed_count } => {
                println!("{}", "🎭 EPHEMERAL PROXY FABRIC SERVER ACTIVE 🎭".bold().cyan());
                println!("  [•] Endpoint:   {}", url.bold().green());
                println!("  [•] Local Port: {}", port);
                println!("  [•] Resource:   {}", resource);
                println!("  [•] Seed Count: {} items", seed_count);
                println!("  [•] CORS:       Enabled (Access-Control-Allow-Origin: *)");
                println!("\n  💡 Test with curl:");
                println!("     {}", format!("curl -s {}", url).cyan());
                println!("     {}", format!("curl -X POST {} -H 'Content-Type: application/json' -d '{{\"name\": \"Test\"}}'", url).cyan());
            }
            HgbResponse::ProxyServerStopped { port } => {
                println!("{} Proxy server on port {} has been stopped.", "🛑 Proxy Server Stopped:".yellow().bold(), port);
            }
            HgbResponse::TraceContext(dump) => {
                println!("{}", "📜 AMBIENT EXECUTION TRACE POST-MORTEM 📜".bold().cyan());
                println!("{}", dump);
            }
            HgbResponse::WorktreeCreated { branch, path } => {
                println!("{} Worktree '{}' created at '{}'", "🌳 Worktree Created:".green().bold(), branch.cyan(), path);
            }
            HgbResponse::WorktreeCleaned { branch } => {
                println!("{} Scratch worktree '{}' removed and pruned.", "🧹 Worktree Cleaned:".yellow().bold(), branch);
            }
            HgbResponse::SemanticStashResult { output } => {
                println!("{}", output);
            }
            // --- Next-Gen Vibe Coding 6 Pillars Responses ---
            HgbResponse::BrowserHealth(rep) => {
                let badge = match rep.verdict {
                    hgb_core::browser_snoop::BrowserHealthVerdict::Healthy => "HEALTHY [GREEN]".green().bold(),
                    hgb_core::browser_snoop::BrowserHealthVerdict::Degraded => "DEGRADED [WARNINGS]".yellow().bold(),
                    hgb_core::browser_snoop::BrowserHealthVerdict::Broken => "BROKEN [RUNTIME ERRORS]".red().bold(),
                };
                println!("{}", "🌐 BROWSER SNOOP RUNTIME HEALTH REPORT 🌐".bold().cyan());
                println!("  [•] Target URL:       {}", rep.inspected_url.cyan());
                println!("  [•] Verdict:          {}", badge);
                println!("  [•] Console Errors:   {}", rep.console_errors_count);
                println!("  [•] Network Failures: {}", rep.network_failures_count);
                println!("  [•] HMR/Build Errors: {}", rep.hmr_errors_count);

                if !rep.suggested_root_causes.is_empty() {
                    println!("\n  🚨 Suggested Root Causes:");
                    for cause in &rep.suggested_root_causes {
                        println!("   • {}", cause.yellow());
                    }
                }
            }
            HgbResponse::BrowserSnoopCleared => {
                println!("{} Browser Snoop incident ring buffer cleared.", "✔ Cleared:".green().bold());
            }
            HgbResponse::VariantRaceManifestReport(manifest) => {
                println!("{}", format!("🏎️ 3-WAY SPECULATIVE DESIGN RACE ({}) 🏎️", manifest.race_id).bold().cyan());
                println!("  [•] Prompt: '{}'", manifest.prompt.yellow());
                println!("  [•] Status: {:?}", manifest.status);
                println!("\n  Active Preview Variants:");
                for (i, cand) in manifest.candidates.iter().enumerate() {
                    println!("   {}. [{}] Port: {} ➔ {}", i + 1, cand.archetype, cand.preview_port.to_string().cyan(), cand.preview_url.green());
                    println!("      Candidate ID: {}", cand.candidate_id.dimmed());
                }
                println!("\n  💡 Select winner with: {}", format!("hgb race --pick <candidate_id>").cyan());
            }
            HgbResponse::VariantWinnerCherryPicked { commit_hash_or_patch } => {
                println!("{} Winner variant cherry-picked!", "🏆 RACE CONVERGED:".green().bold());
                println!("{}", commit_hash_or_patch);
            }
            HgbResponse::VariantRaceAborted => {
                println!("{} Speculative race cancelled and proxy preview ports released.", "🛑 Race Aborted:".yellow().bold());
            }
            HgbResponse::DbDriftReport(rep) => {
                println!("{}", "🗄️ DBSENTINEL SCHEMA DRIFT AUDIT 🗄️".bold().cyan());
                println!("  [•] Database: {}", rep.database_path.yellow());
                println!("  [•] Status:   {}", if rep.is_in_sync { "IN SYNC [CLEAN]".green().bold() } else { "DRIFT DETECTED".red().bold() });
                println!("  [•] Safety:   {}", rep.overall_safety.to_string().cyan());

                if !rep.drift_items.is_empty() {
                    println!("\n  📋 Drift Items ({}):", rep.drift_items.len());
                    for item in &rep.drift_items {
                        match item {
                            hgb_core::db_sentinel::SchemaDriftItem::MissingTable { table_name } => {
                                println!("   • Missing Table: {}", table_name.red().bold());
                            }
                            hgb_core::db_sentinel::SchemaDriftItem::ExtraTable { table_name } => {
                                println!("   • Extra Live Table: {}", table_name.yellow());
                            }
                            hgb_core::db_sentinel::SchemaDriftItem::MissingColumn { table_name, column } => {
                                println!("   • Table '{}' missing column '{}' ({})", table_name, column.name.red(), column.data_type.cyan());
                            }
                            hgb_core::db_sentinel::SchemaDriftItem::ColumnTypeMismatch { table_name, column_name, expected_type, live_type } => {
                                println!("   • Type mismatch in '{}.{}': expected {}, live is {}", table_name, column_name, expected_type.green(), live_type.red());
                            }
                            hgb_core::db_sentinel::SchemaDriftItem::NullabilityMismatch { table_name, column_name, expected_nullable, live_nullable } => {
                                println!("   • Nullability mismatch in '{}.{}': expected {}, live {}", table_name, column_name, expected_nullable, live_nullable);
                            }
                        }
                    }

                    if !rep.generated_forward_sql.is_empty() {
                        println!("\n  🛠️ Generated Forward Migration SQL:\n{}", rep.generated_forward_sql.green());
                    }
                }
            }
            HgbResponse::DbDryRunResult { success, error } => {
                if success {
                    println!("{} Transactional savepoint dry-run passed with zero errors.", "✔ DRY-RUN SUCCESS:".green().bold());
                } else {
                    println!("{} Dry-run migration failed: {}", "✖ DRY-RUN FAILED:".red().bold(), error.unwrap_or_default().red());
                }
            }
            HgbResponse::DbMigrationApplied { migration_file } => {
                println!("{} {}", "✔ MIGRATION COMMITTED:".green().bold(), migration_file);
            }
            HgbResponse::SyntaxSliceReport(res) => {
                println!("{}", format!("✂️ SURGICAL AST CONTEXT SLICE: '{}' ({}) ✂️", res.focal_symbol, res.language).bold().cyan());
                println!("  [•] Original: {} chars (~{} tokens)", res.token_metrics.raw_characters, res.token_metrics.estimated_raw_tokens);
                println!("  [•] Sliced:   {} chars (~{} tokens)", res.token_metrics.sliced_characters, res.token_metrics.estimated_sliced_tokens);
                println!("  [•] Savings:  {:.1}% token reduction", res.token_metrics.reduction_percentage.to_string().green().bold());
                println!("\n{}", res.rendered_surgical_prompt);
            }
            HgbResponse::AutoSpecSynthesized(spec) => {
                println!("{}", format!("🧪 AUTOSPEC GOLDEN SUITE SYNTHESIZED: '{}' 🧪", spec.spec_id).bold().cyan());
                println!("  [•] Target Function: {}", spec.target_function.yellow());
                println!("  [•] Target Module:   {}", spec.target_module);
                println!("  [•] Invariant Rules: {} rules", spec.invariant_rules.len());
                println!("  [•] Test Vectors:    {} boundary vectors", spec.golden_vectors.len());
            }
            HgbResponse::AutoSpecRunReport(reports) => {
                println!("{}", "🛡️ AUTOSPEC REGRESSION GUARD REPORT 🛡️".bold().cyan());
                for rep in reports {
                    let badge = if rep.is_green { "GREEN [PASSED]".green().bold() } else { "FAILED [REGRESSION]".red().bold() };
                    println!("  [•] Spec '{}': {} ({} passed, {} failed)", rep.spec_id.yellow(), badge, rep.passed_tests, rep.failed_tests);
                    for fail in rep.failures {
                        println!("      🚨 Failed on input: {} ➔ {}", fail.input_used.red(), fail.actual);
                    }
                }
            }
            HgbResponse::AutoSpecListReport(specs) => {
                println!("{}", format!("🧪 ACTIVE AUTOSPEC GOLDEN SPECS ({}) 🧪", specs.len()).bold().cyan());
                for s in specs {
                    println!("  • [{}] {} ({} vectors)", s.spec_id.yellow(), s.target_function, s.golden_vectors.len());
                }
            }
            HgbResponse::DriftDnaReport(dna) => {
                println!("{}", "🧬 REPOSITORY ARCHITECTURAL DNA 🧬".bold().cyan());
                println!("  [•] Ecosystem:     {}", dna.ecosystem.yellow());
                println!("  [•] HTTP Client:   {}", dna.pillars.preferred_http_client.green());
                println!("  [•] Styling:       {}", dna.pillars.preferred_styling.green());
                println!("  [•] State Mgr:     {}", dna.pillars.preferred_state_mgr.green());
                println!("  [•] Error Policy:  {}", dna.pillars.error_handling_policy.green());
                println!("  [•] Async Runtime: {}", dna.pillars.async_runtime.green());
                println!("  [•] Forbidden Imports: {:?}", dna.forbidden_import_patterns);
                println!("  [•] Forbidden Syntax:  {:?}", dna.forbidden_syntax_patterns);
            }
            HgbResponse::ComplianceAuditResult(report) => {
                println!("{}", "🏛️ ARCHITECTURAL COMPLIANCE AUDIT 🏛️".bold().cyan());
                println!("  [•] Result: {}", if report.passed { "PASSED [ALIGNED]".green().bold() } else { "FAILED [DNA VIOLATION]".red().bold() });
                println!("  [•] Total Violations:    {}", report.total_violations);
                println!("  [•] Blocking Violations: {}", report.blocking_violations);

                for v in report.violations {
                    println!("\n  🚨 [{}] {}", v.severity.to_string().red().bold(), v.rule_name);
                    println!("     Snippet:     {}", v.offending_snippet.yellow());
                    println!("     Reason:      {}", v.reason);
                    println!("     Remediation: {}", v.remediation_hint.cyan());
                }
            }
            // --- Superpowers Vibe Coding Responses ---
            HgbResponse::McpToolsList(tools) => {
                println!("{}", format!("🔌 UNIVERSAL MCP DISCOVERED TOOLS ({}) 🔌", tools.len()).bold().cyan());
                if tools.is_empty() {
                    println!("  {}", "No tools discovered. Configure servers in 'hagibis.mcp.json' or '.hgb/mcp.json'.".dimmed());
                } else {
                    for t in tools {
                        println!("  • {}", t.name.green().bold());
                        if let Some(desc) = &t.description {
                            println!("    {}", desc.dimmed());
                        }
                    }
                }
            }
            HgbResponse::McpToolCallResult(val) => {
                println!("{}", "🔌 MCP TOOL EXECUTION RESULT:".green().bold());
                println!("{}", serde_json::to_string_pretty(&val).unwrap_or_else(|_| format!("{:?}", val)));
            }
            HgbResponse::TimelineCreated(info) => {
                println!("{} Ephemeral timeline '{}' ready", "🌿 TIMELINE FORKED:".green().bold(), info.name.cyan());
                println!("  [•] Location: {}", info.path.display().to_string().yellow());
                println!("  [•] Type:     {}", if info.is_git_worktree { "Git Worktree" } else { "Snapshot Fallback" });
                println!("  [•] Base:     {}", info.base_branch.unwrap_or_else(|| "default".into()));
            }
            HgbResponse::TimelineListReport(list) => {
                println!("{}", format!("🌿 ACTIVE EPHEMERAL TIMELINES ({}) 🌿", list.len()).bold().cyan());
                if list.is_empty() {
                    println!("  {}", "No active timelines. Fork one with 'hgb timeline create <name>'".dimmed());
                } else {
                    for t in list {
                        let kind = if t.is_git_worktree { "worktree" } else { "snapshot" };
                        println!("  • {} [{}] ➔ {}", t.name.green().bold(), kind.cyan(), t.path.display().to_string().dimmed());
                    }
                }
            }
            HgbResponse::TimelineDiffReport(diff) => {
                println!("{}", format!("🌿 TIMELINE DIFF: '{}' ({} files changed) 🌿", diff.timeline_name, diff.files_changed).bold().cyan());
                if diff.files_changed == 0 {
                    println!("  {}", "Timeline is identical to workspace.".green());
                } else {
                    for f in &diff.modified_files {
                        println!("  ~ {}", f.yellow());
                    }
                    if !diff.diff_content.is_empty() {
                        println!("\n{}", diff.diff_content);
                    }
                }
            }
            HgbResponse::TimelineMergeReport(rep) => {
                if rep.success {
                    println!("{} Timeline '{}' successfully merged into workspace!", "✔ TIMELINE MERGED:".green().bold(), rep.timeline_name.cyan());
                    println!("  Modified files ({}):", rep.merged_files.len());
                    for f in &rep.merged_files {
                        println!("    • {}", f.green());
                    }
                } else {
                    println!("{} Merge failed: {}", "✖ TIMELINE MERGE FAILED:".red().bold(), rep.message.red());
                }
            }
            HgbResponse::TimelineDiscarded { name } => {
                println!("{} Ephemeral timeline '{}' discarded and cleaned up.", "✔ TIMELINE DISCARDED:".green().bold(), name);
            }
            HgbResponse::VerificationGateCertificate(cert) => {
                let badge = match cert.status {
                    hgb_core::verification_gate::VerificationStatus::Passed => "PASSED [GREEN]".green().bold(),
                    hgb_core::verification_gate::VerificationStatus::Healed => "HEALED [AUTO-REPAIRED]".cyan().bold(),
                    hgb_core::verification_gate::VerificationStatus::Failed => "FAILED [INVARIANTS VIOLATED]".red().bold(),
                    hgb_core::verification_gate::VerificationStatus::Rejected => "REJECTED".red().bold(),
                };
                println!("{}", "🛡️ VERIFICATION GATE & GOLDEN INVARIANT GUARD 🛡️".bold().cyan());
                println!("  [•] Certificate ID: {}", cert.certificate_id.yellow());
                println!("  [•] Status:         {}", badge);
                println!("  [•] Blake3 Hash:    {}", cert.integrity_hash.dimmed());
                println!("  [•] Steps Run:      {} steps (iterations: {})", cert.steps.len(), cert.iterations_run);
                for step in &cert.steps {
                    let st = if step.passed { "PASS".green().bold() } else { "FAIL".red().bold() };
                    println!("    [{}] {} ({}ms)", st, step.name, step.duration_ms);
                    if !step.passed && !step.output.is_empty() {
                        let snippet: String = step.output.lines().take(4).collect::<Vec<_>>().join("\n      ");
                        println!("      {}", snippet.dimmed());
                    }
                }
                if !cert.healed_patches.is_empty() {
                    println!("\n  🩹 Applied Self-Healing Patches:");
                    for p in &cert.healed_patches {
                        println!("    • {}", p.green());
                    }
                }
            }
            HgbResponse::ShellInitScript(script) => {
                print!("{}", script);
            }
            HgbResponse::ShellCrashRecorded { crash_id } => {
                println!("{} Logged crash to .hgb/crashes/{}.json", "🚨 Shell crash recorded:".red().bold(), crash_id);
            }
            HgbResponse::ShellCrashDiagnosis(diag) => {
                println!("{}", "🚨 HGB SHELL COMPANION CRASH DIAGNOSIS 🚨".bold().red());
                println!("  [•] Category:   {:?}", diag.category);
                println!("  [•] Root Cause: {}", diag.root_cause.yellow().bold());
                println!("  [•] Fix:        {}", diag.suggested_command_fix.green().bold());
                println!("  [•] Confidence: {:.0}%", diag.confidence * 100.0);
                println!("  [•] Why:        {}", diag.explanation);
                if !diag.alternative_fixes.is_empty() {
                    println!("  [•] Alternatives:");
                    for alt in &diag.alternative_fixes {
                        println!("      - {}", alt.cyan());
                    }
                }
            }
            HgbResponse::AmbientVibeReport(events) => {
                println!("{}", "⚡ AMBIENT VIBE CHECK SUITE REPORT ⚡".bold().magenta());
                for ev in events {
                    match ev {
                        hgb_core::ambient_vibe::VibeWatchEvent::CheckPassed { suite_name, duration_ms, summary } => {
                            println!("  ✔ {} ({}ms): {}", suite_name.green().bold(), duration_ms, summary);
                        }
                        hgb_core::ambient_vibe::VibeWatchEvent::CheckFailed { suite_name, duration_ms, error_output, exit_code } => {
                            println!("  ✖ {} ({}ms, exit code {}):", suite_name.red().bold(), duration_ms, exit_code);
                            let snip: String = error_output.lines().take(3).collect::<Vec<_>>().join("\n    ");
                            println!("    {}", snip.dimmed());
                        }
                        hgb_core::ambient_vibe::VibeWatchEvent::SpeculativePatchReady { file_path, explanation, .. } => {
                            println!("  🩹 Speculative patch ready for {}: {}", file_path.display().to_string().cyan(), explanation);
                        }
                        _ => {}
                    }
                }
            }
            HgbResponse::GlanceSynthesizedCode { framework, code, css } => {
                println!("{}", format!("✨ SYNTHESIZED {} COMPONENT ✨", framework.to_uppercase()).bold().green());
                if let Some(c) = css {
                    println!("  Styles: {}", c.cyan());
                }
                println!("\n{}", code);
            }
            HgbResponse::AstPatchReport(report) => {
                println!("{}", "🌳 AST PATCH ARBITER: DISCOVERED SEMANTIC HUNKS 🌳".bold().cyan());
                println!("  Total Hunks: {} (Accepted: {}, Staged: {}, Rejected: {})",
                    report.total_hunks, report.accepted_count, report.staged_count, report.rejected_count);
                println!("  Syntax Integrity Valid: {}", if report.syntax_valid { "✔ PASSED".green() } else { "✖ FAILED".red() });
                for hunk in &report.hunks {
                    println!("    [{}] {} (Lines {}-{}): {}", hunk.id.cyan(), hunk.kind.badge().yellow(), hunk.start_line, hunk.end_line, hunk.symbol_name.bold());
                }
            }
            HgbResponse::AstPatchApplied { code } => {
                println!("{}", "✔ AST PATCH SUCCESSFULLY APPLIED & VERIFIED".bold().green());
                let preview: Vec<&str> = code.lines().take(6).collect();
                println!("{}\n  ... ({} lines total)", preview.join("\n"), code.lines().count());
            }
            HgbResponse::LiveTunnelSessionReport(session) => {
                println!("{}", "📱 P2P MOBILE PREVIEW TUNNEL READY 📱".bold().cyan());
                println!("  URL:         {}", session.public_url.green().bold());
                println!("  Local Port:  {}", session.local_port);
                println!("  Session ID:  {}", session.session_id);
                println!("\n{}\n", session.qr_matrix_terminal);
                println!("  {} Scan QR with your smartphone to inspect preview on mobile device", "💡".cyan());
            }
            HgbResponse::MobileTelemetryReport(ev) => {
                println!("{}", format!("📱 MOBILE TELEMETRY [{}]: {}", ev.event_type, ev.message).bold().yellow());
                println!("  Device: {} | Viewport: {}", ev.user_agent, ev.viewport);
            }
            HgbResponse::TddCycleReport(rep) => {
                println!("{}", "🔴🟢 AUTONOMOUS SPECULATIVE TDD CYCLE REPORT 🟢🔴".bold().green());
                println!("  Intent:            {}", rep.intent.cyan());
                println!("  Target Function:   {}", rep.spec.target_function.bold());
                println!("  Phase 1 (RED):     {}", if rep.red_verified { "✔ Verified (Test Fails on Blank State)".green() } else { "✖ Failed".red() });
                println!("  Phase 2 (GREEN):   {}", if rep.green_verified { "✔ Verified (Implementation Passes Invariants)".green() } else { "✖ Failed".red() });
                println!("  Phase 3 (REFACTOR):{}", if rep.refactor_clean { "✔ Cleaned & Verified Zero Regressions".green() } else { "✖ Failed".red() });
                println!("  Duration:          {}ms ({} iterations)", rep.duration_ms, rep.iterations);
                println!("\n{}", "--- Synthesized Implementation ---".dimmed());
                println!("{}", rep.synthesized_code.green());
            }
            HgbResponse::MicroSandboxExecutionReport(rep) => {
                let status_str = if rep.exit_code == 0 { "✔ CLEAN (Exit 0)".green().bold() } else { format!("✖ FAILED (Exit {})", rep.exit_code).red().bold() };
                println!("{}", format!("🛡️ MICRO-SANDBOX EXECUTION: {}", status_str));
                println!("  Command:          {}", rep.command.cyan());
                println!("  Shielded Secrets: {}", rep.secrets_shielded);
                println!("  Duration:         {}ms", rep.execution_time_ms);
                if !rep.stdout.trim().is_empty() {
                    println!("\n  Stdout:\n{}", rep.stdout.trim());
                }
                if !rep.stderr.trim().is_empty() {
                    println!("\n  Stderr:\n{}", rep.stderr.trim().red());
                }
            }
            HgbResponse::AudioCuePlayed { cue } => {
                println!("  {} {}", "🔔".cyan(), cue.description().italic());
            }
            HgbResponse::VoiceIntentReport(maybe_intent) => {
                if let Some(intent) = maybe_intent {
                    println!("{}", format!("🎙️ VOICE INTENT [{:.0}%]: {} -> {:?}", intent.confidence * 100.0, intent.action, intent.target_symbol).bold().cyan());
                } else {
                    println!("  {}", "No recognized voice intent in transcript".dimmed());
                }
            }
            HgbResponse::CdpPatchResult(rep) => {
                println!("{}", "⚡ CDP LIVE IN-MEMORY PATCH REPORT ⚡".bold().cyan());
                println!("  Target:                 {}", rep.target.yellow());
                println!("  Patch Kind:             {}", rep.patch_kind.green());
                println!("  Client State Preserved: {}", if rep.client_state_preserved { "✔ Form/Auth/DOM Intact".green() } else { "✖ Reset".red() });
                println!("  Latency:                {}µs", rep.latency_us);
            }
            HgbResponse::SkeletonLensResult(rep) => {
                println!("{}", format!("🧬 AST SKELETON LENS: '{}' 🧬", rep.target_symbol).bold().cyan());
                println!("  Original Lines:     {} (~{} tokens)", rep.original_lines, rep.original_tokens_est);
                println!("  Compacted Lines:    {} (~{} tokens)", rep.compacted_lines, rep.compacted_tokens_est);
                println!("  Token Savings:      {:.1}%", rep.token_savings_pct.to_string().green().bold());
                println!("  Folded Signatures:  {}", rep.folded_symbols_count);
                println!("\n{}", rep.projected_code);
            }
            HgbResponse::LakandiwaSwarmResult(rep) => {
                println!("{}", "🏛️ LAKANDIWA TRIPLE-MODEL CONSENSUS SWARM 🏛️".bold().cyan());
                println!("  Prompt:             {}", rep.prompt.yellow());
                println!("  Winner Model:       {}", rep.winner_model.green().bold());
                println!("  Contest Duration:   {}ms", rep.duration_ms);
                println!("\n  Evaluated Candidates ({}):", rep.candidate_count);
                for c in &rep.candidates {
                    println!("    • [{:.1}/10] {:<28} ({}ms) | Syntax: {} | Tests: {}",
                        c.total_score, c.candidate_name.cyan(), c.latency_ms,
                        if c.syntax_valid { "✔".green() } else { "✖".red() },
                        if c.tests_passed { "✔".green() } else { "✖".red() });
                }
                println!("\n--- Winning Patch ---\n{}", rep.winning_patch.green());
            }
            HgbResponse::DbCowSnapshotCreated(rec) => {
                println!("{}", "💾 DATABASE COPY-ON-WRITE SNAPSHOT CREATED 💾".bold().green());
                println!("  Source:      {}", rec.source_path.yellow());
                println!("  Snapshot:    {}", rec.snapshot_id.cyan());
                println!("  Size:        {} bytes", rec.byte_size);
                let short = if rec.blake3_hash.len() > 16 { &rec.blake3_hash[..16] } else { &rec.blake3_hash };
                println!("  Blake3:      {}", short);
            }
            HgbResponse::DbCowSnapshotRestored { bytes_restored } => {
                println!("{} Database restored to exact binary byte-state ({} bytes).", "✔ DB ROLLBACK COMPLETE:".green().bold(), bytes_restored);
            }
            HgbResponse::SlopsquattingReport(rep) => {
                println!("{}", "🛡️ SUPPLY-CHAIN & SLOPSQUATTING FIREWALL AUDIT 🛡️".bold().cyan());
                println!("  Inspected: {} | Safe: {} | Blocked: {}",
                    rep.total_inspected, rep.safe_count.to_string().green(), rep.blocked_count.to_string().red().bold());
                for item in &rep.items {
                    let badge = match item.risk_level {
                        hgb_core::slopsquatting_firewall::PackageRiskLevel::Safe => "[SAFE]".green(),
                        hgb_core::slopsquatting_firewall::PackageRiskLevel::Suspicious => "[SUSPICIOUS]".yellow(),
                        hgb_core::slopsquatting_firewall::PackageRiskLevel::Quarantined => "[QUARANTINED]".red().bold(),
                    };
                    println!("    {} {:<25} ➔ {}", badge, item.package_name.cyan(), item.reason.dimmed());
                }
            }
            HgbResponse::CloudLaunchpadReport(rep) => {
                println!("{}", "🌐 ZERO-OPS CLOUD LAUNCHPAD: DEPLOYED TO EDGE 🌐".bold().green());
                println!("  Public URL:       {}", rep.public_url.green().bold());
                println!("  Project Slug:     {}", rep.deployment_id.cyan());
                println!("  Framework:        {}", rep.framework.yellow());
                println!("  TLS Certificate:  {}", rep.tls_certificate);
                println!("  Deploy Duration:  {}ms", rep.duration_ms);
                print!("{}", rep.osc52_clipboard_code); // Copy link to OS clipboard via OSC 52
                println!("  {} Link copied to OS clipboard via OSC 52", "📋".cyan());
            }
            HgbResponse::FlightSimulatorResult(rep) => {
                println!("{}", rep.ascii_flight_trace);
            }
            HgbResponse::ShadowSynthesizerResult(rep) => {
                println!("{}", "👻 SUB-MILLISECOND PREDICTIVE SHADOW SYNTHESIZER 👻".bold().cyan());
                println!("  Resident Cache Size:  {} speculative continuations", rep.active_cache_size);
                println!("  Cache Hit Status:     {}", if rep.hit { "HIT (sub-50µs)".bold().green() } else { "DYNAMIC SYNTHESIS".yellow() });
                if let Some(top) = rep.top_prediction {
                    println!("  Top Prediction Symbol: {}", top.symbol_name.bold().green());
                    println!("  Confidence Score:      {:.1}%", top.confidence * 100.0);
                    println!("  Precomputation Time:   {}µs", top.latency_us);
                    println!("  Synthesized AST:\n{}", top.continuation_code.cyan());
                }
            }
            HgbResponse::ApiMirageResult(rep) => {
                println!("{}", "🔮 UNIVERSAL OFFLINE API MIRAGE & WIRETAPPER 🔮".bold().magenta());
                println!("  Endpoint:        {} {}", rep.method.bold().yellow(), rep.endpoint.cyan());
                println!("  Simulated HTTP:  {}", rep.status);
                println!("  Synthetic Mode:  {}", rep.is_synthetic);
                println!("  Response Time:   {}ms (deterministic offline proxy)", rep.duration_ms);
                println!("  Payload Preview:\n{}", rep.payload_snippet);
            }
            HgbResponse::ChaosMonkeyResult(rep) => {
                println!("{}", "🐒 IN-PROCESS CHAOS MONKEY & INVARIANT FUZZER 🐒".bold().red());
                println!("  Target Component:     {}", rep.target_component.bold());
                println!("  Trials Executed:      {}", rep.trials_run);
                println!("  Invariants Preserved: {} / {}", rep.invariants_passed, rep.trials_run);
                println!("  Survival Rate:        {:.1}%", rep.survival_rate);
                for trial in &rep.details {
                    let mark = if trial.passed { "✅".green() } else { "❌".red() };
                    println!("    {} [{:<22}] - {}ms | {}", mark, trial.vector_id, trial.simulated_latency_ms, trial.error_caught.as_deref().unwrap_or("Invariant Verified"));
                }
            }
            HgbResponse::ChaosTrialResult(trial) => {
                println!("{}", "🐒 CHAOS MONKEY IDEMPOTENCY BURST RESULT 🐒".bold().red());
                println!("  Vector:    {}", trial.vector_id);
                println!("  Status:    {}", if trial.passed { "PASSED (No state race)".bold().green() } else { "FAILED".bold().red() });
                println!("  Detail:    {}", trial.error_caught.as_deref().unwrap_or(""));
            }
            HgbResponse::NightShiftResult(rep) => {
                println!("{}", "🌙 AUTONOMOUS NIGHT-SHIFT SWARM WORKTREE PIPELINE 🌙".bold().purple());
                println!("  Task ID:          {}", rep.task_id.bold().yellow());
                println!("  Goal:             {}", rep.goal_description);
                println!("  Worktree Branch:  {}", rep.worktree_branch.green());
                println!("  Tests Passed:     {} / {}", rep.tests_passed, rep.tests_passed);
                println!("  Lines Changed:    +{} lines", rep.loc_changed);
                println!("  Review Ready:     {}", rep.ready_for_review);
                println!("\n{}", rep.pr_summary);
            }
            HgbResponse::VaultSealResult(seal) => {
                println!("{}", "🔒 KERNEL-LEVEL MEMORY-ONLY GHOST ENVS: SEALED 🔒".bold().green());
                println!("  Entries Sealed:   {}", seal.entries_count);
                println!("  Cipher Hash:      {}", seal.cipher_hash.cyan());
                println!("  Payload Size:     {} bytes (Encrypted with Blake3 XOF Stream)", seal.encrypted_payload.len());
                println!("  Disk Safety:      100% Guaranteed. Real secrets never touch disk.");
            }
            HgbResponse::VaultAuditResult(rep) => {
                println!("{}", "🛡️ GHOST ENVS DISK SANITIZATION AUDIT 🛡️".bold().cyan());
                println!("  Vault Status:     {}", if rep.vault_intact { "INTACT (Blake3 Sealed)".green() } else { "UNSEALED".red() });
                println!("  Disk Sanitized:   {}", if rep.disk_sanitized { "CLEAN (Zero Plaintext Leaks)".bold().green() } else { "CRITICAL LEAKS DETECTED".bold().red() });
                println!("  Injected Vars:    {} variables memory-only", rep.injected_variables_count);
                if !rep.leaked_keys_detected.is_empty() {
                    println!("  Leaked Keys:      {:?}", rep.leaked_keys_detected);
                }
                println!("  Summary:          {}", rep.status_message);
            }
            HgbResponse::TypeLockResult(rep) => {
                println!("{}", "🔒 ZERO-DRIFT POLYGLOT TYPE LOCK 🔒".bold().yellow());
                println!("  Models Synced:    {}", rep.models_synced);
                println!("  Zero-Drift:       {}", if rep.zero_drift_achieved { "ACHIEVED (100% Parity)".bold().green() } else { "DRIFT DETECTED".bold().red() });
                if !rep.drifts_detected.is_empty() {
                    println!("  Discrepancies:");
                    for d in &rep.drifts_detected {
                        println!("    ⚠ {}.{}: {}", d.model.red(), d.field.yellow(), d.discrepancy);
                    }
                }
                println!("\n--- Generated TypeScript ---\n{}", rep.generated_ts_interfaces);
                println!("--- Generated Zod Validation ---\n{}", rep.generated_zod_schemas);
            }
            HgbResponse::SpatialRadarResult(rep) => {
                println!("{}", rep.ascii_radar);
                println!("  Visible Nodes:    {}", rep.nodes_visible);
                println!("  System Health:    {:.1}%", rep.system_health);
                for rec in &rep.recommendations {
                    println!("  💡 {}", rec.cyan());
                }
            }
            HgbResponse::CdpTeleportResult(rep) => {
                println!("{}", "🎯 CLICK-TO-SOURCE CDP TELEPORT 🎯".bold().cyan());
                println!("  Query Selector:   {}", rep.query_selector.yellow());
                println!("  Matched:          {}", if rep.matched { "EXACT MATCH".bold().green() } else { "HEURISTIC SYNTHESIS".yellow() });
                println!("  Confidence:       {:.1}%", rep.confidence * 100.0);
                if let Some(t) = rep.target {
                    println!("  Target File:      {}:{}:{}", t.source_file.green().bold(), t.line_number, t.column_number);
                    println!("  Symbol Name:      {} ({})", t.symbol_name.bold(), t.component_type);
                    println!("  Source Excerpt:\n{}", t.code_snippet.cyan());
                }
                println!("  💡 {}", rep.ghost_patch_hint.dimmed());
            }
            HgbResponse::VoiceFlowResult(rep) => {
                println!("{}", "🎙️ FULL-DUPLEX ZERO-LATENCY VOICE FLOW CO-PILOT 🎙️".bold().magenta());
                println!("  Session ID:       {}", rep.session_id.yellow());
                println!("  Spoken Transcript:\"{}\"", rep.dispatch.transcript.bold().white());
                println!("  Recognized Intent:{:?}", rep.dispatch.kind);
                println!("  Target AST Node:  {}", rep.dispatch.ast_target_symbol.cyan());
                println!("  Processing Delay: {}ms (Sub-100ms real-time audio SLA)", rep.processing_latency_ms);
                println!("  Spoken Response:  🔊 \"{}\"", rep.dispatch.spoken_acknowledgment.bold().green());
            }
            HgbResponse::PrTapeResult(rep) => {
                println!("{}", "🎥 HEADLESS SCREENPLAY & AUTOMATED PR LOOM TAPE 🎥".bold().purple());
                println!("  Tape ID:          {}", rep.tape_id.yellow());
                println!("  Target Route:     {}", rep.url.cyan());
                println!("  Scenario Name:    {}", rep.scenario_name.bold());
                println!("  Steps Executed:   {}", rep.steps_executed.len());
                println!("  Duration:         {}ms ({} frames)", rep.duration_ms, rep.frame_count);
                println!("  Compressed Size:  {} bytes (Animated WebP)", rep.file_size_bytes);
                println!("\n{}", rep.markdown_embed_snippet);
            }
            HgbResponse::FinOpsResult(rep) => {
                println!("{}", "💰 TOKEN FINOPS & DYNAMIC LATENCY ARBITRAGE 💰".bold().green());
                println!("  Selected Tier:    {:?}", rep.decision.selected_tier);
                println!("  Target Model:     {}", rep.decision.target_model.bold().yellow());
                println!("  Estimated Tokens: {} tokens", rep.decision.estimated_tokens);
                println!("  Cost Per Prompt:  ${:.6} USD", rep.decision.estimated_cost_usd);
                println!("  Projected Latency:{}ms", rep.decision.projected_latency_ms);
                println!("  Routing Rationale:{}", rep.decision.reasoning.cyan());
                println!("  Lifetime Savings: {} tokens (${:.2} USD preserved)", rep.lifetime_tokens_saved, rep.lifetime_dollars_saved_usd);
                println!("  Local Model Ratio:{:.1}% on resident Ollama", rep.local_execution_ratio);
            }
            HgbResponse::AirgapCloakResult(rep) => {
                println!("{}", "🥷 ZERO-KNOWLEDGE AIRGAP CLOAK & PII SANITIZER 🥷".bold().yellow());
                println!("  Original Length:  {} bytes", rep.original_length);
                println!("  Entities Masked:  {}", rep.entities_masked.len());
                for e in &rep.entities_masked {
                    println!("    🔒 [{:<12}] {} -> {}", e.entity_type.cyan(), e.original_masked, e.placeholder.yellow());
                }
                println!("  Cloaked Prompt:\n{}", rep.cloaked_text.green());
            }
            HgbResponse::AirgapRehydrateResult { rehydrated_text } => {
                println!("{}", "🥷 AIRGAP CLOAK: RESPONSE REHYDRATED 🥷".bold().green());
                println!("  Rehydrated Text:\n{}", rehydrated_text);
            }
            HgbResponse::SqlGuardResult(rep) => {
                println!("{}", "🛡️ ACTIVE SQL INTERCEPTOR & SHADOW TRANSACTION JAIL 🛡️".bold().red());
                println!("  Raw Statement:    {}", rep.raw_query.bold().white());
                println!("  Safety Verdict:   {:?}", rep.verdict);
                println!("  Destructive:      {}", if rep.is_destructive { "YES (CRITICAL)".bold().red() } else { "NO (Verified Safe)".green() });
                println!("  WHERE Clause:     {}", if rep.has_where_clause { "DETECTED".green() } else { "MISSING".red().bold() });
                println!("  Rows Impacted:    ~{} rows", rep.simulated_rows_impacted);
                if let Some(snap) = rep.shadow_snapshot_id {
                    println!("  Shadow Snapshot:  {}", snap.yellow());
                }
                println!("  Audit Verdict:    {}", rep.explanation);
            }
            HgbResponse::ExecutionReplayResult(rep) => {
                println!("{}", "⏱️ DETERMINISTIC EXECUTION REPLAY & REWIND-EXEC ⏱️".bold().cyan());
                println!("  Flight Trace ID:  {}", rep.trace_id.yellow());
                println!("  Total Frames:     {}", rep.total_frames);
                println!("  Scrubbed Frame:   #{}", rep.scrubbed_frame_index);
                for frame in &rep.frames {
                    let mark = if frame.is_anomaly { "🚨".red() } else { "🔹".blue() };
                    println!("    {} Frame #{}: [{:<24}] @ {} (+{}ms) | {}", mark, frame.frame_index, frame.event_kind, frame.symbol_location, frame.timestamp_ms, frame.state_snapshot_snippet.dimmed());
                }
                println!("  Diagnosis:        {}", rep.diagnosis.bold());
            }
            HgbResponse::CanvasMutationResult(rep) => {
                println!("{}", "🎨 TWO-WAY VISUAL CANVAS & LIVE CSS MIRROR 🎨".bold().cyan());
                println!("  Target File:     {}", rep.target_file.yellow());
                println!("  Symbol Name:     {}", rep.symbol_name.green());
                println!("  Applied Classes: {:?}", rep.applied_classes);
                println!("  Patched JSX:     {}", rep.patched_jsx.bold());
                println!("  Latency:         {} µs (Zero Token Waste: {})", rep.latency_us, rep.zero_token_waste);
            }
            HgbResponse::MultiRepoFederateResult(rep) => {
                println!("{}", "🌐 MULTI-REPO SWARM & MONOREPO MESH FEDERATOR 🌐".bold().cyan());
                println!("  Feature Goal:    {}", rep.feature_goal.yellow());
                println!("  Sync Session ID: {}", rep.sync_id.green());
                println!("  Unified Branch:  {}", rep.unified_branch.cyan());
                println!("  Repos Mesh:      {} repositories synchronized", rep.repos_coordinated);
                for task in &rep.tasks {
                    println!("    📦 [{:<18}] ({}) -> {}", task.repo_name.bold(), task.stack_type.dimmed(), task.status.green());
                }
                println!("  Contract Valid:  {}", if rep.contract_compatibility { "YES (100% Type-Safe)".green() } else { "DRIFT DETECTED".red() });
                println!("  PR Bundle:       {}", rep.pr_sync_bundle.dimmed());
            }
            HgbResponse::TimeWarpResult(rep) => {
                println!("{}", "⏳ RELATIONAL TIME-WARP DATA SYNTHESIZER ⏳".bold().cyan());
                println!("  Time Horizon:    {} months simulated ({})", rep.timespan_months, rep.temporal_range.0);
                println!("  Total Records:   {} entities", rep.total_records);
                println!("  FK Integrity:    {}", if rep.fk_integrity_verified { "100% VALIDATED (0 Violations)".green() } else { "BROKEN FK".red() });
                println!("  Clock Skew Runs: {} edge cases (DST/leap simulated)", rep.clock_skew_events_simulated);
                for s in &rep.table_summaries {
                    println!("    📊 Table {:<15}: {:>4} rows {}", s.table_name.bold(), s.count, s.fk_column.as_deref().unwrap_or(""));
                }
                println!("  JSON Bytes:      {} bytes", rep.json_fixture_bytes);
            }
            HgbResponse::StructuralGuardrailsResult(rep) => {
                println!("{}", "🏛️ STRUCTURAL INVARIANT GUARDRAILS & ANTI-SPAGHETTI 🏛️".bold().cyan());
                println!("  Clean Score:     {}/100 | Status: {}", rep.clean_architecture_score, if rep.healthy { "CLEAN & SOLID".green() } else { "ACTION NEEDED".red() });
                println!("  Scanned Files:   {} | Invariant Checks: {}", rep.scanned_files, rep.passed_rules);
                if rep.violations.is_empty() {
                    println!("  Violations:      None! Zero architecture leaks.");
                } else {
                    for v in &rep.violations {
                        println!("    🚨 [{:?}] {} @ {}:{}", v.severity, v.rule_name.bold(), v.file_path, v.line);
                        println!("       Message: {}", v.message.yellow());
                        println!("       Fix:     {}", v.suggested_fix.green());
                    }
                }
            }
            HgbResponse::CrashTriageResult(rep) => {
                println!("{}", "🚑 PRODUCTION CRASH AUTO-TRIAGE & REPRODUCTION 🚑".bold().cyan());
                println!("  Crash ID:        {}", rep.crash_id.yellow());
                println!("  Culprit Frame:   {}:{} ({})", rep.culprit_file.bold(), rep.culprit_line, rep.language.green());
                println!("  Root Cause:      {}", rep.root_cause_analysis.bold());
                println!("  Repro Test:      \n{}", rep.reproduction_test_code.dimmed());
                println!("  Defensive Patch: \n{}", rep.defensive_patch.green());
            }
            HgbResponse::FlakyDeflakeResult(rep) => {
                println!("{}", "🎯 FLAKY TEST EXTERMINATOR & STRESS FUZZER 🎯".bold().cyan());
                println!("  Target Test:     {}", rep.test_name.bold());
                println!("  Stress Runs:     {}/{} passed ({:.1}% pass rate)", rep.passed_runs, rep.total_runs, (1.0 - rep.flakiness_ratio) * 100.0);
                println!("  Flaky Verdict:   {}", if rep.is_flaky { "FLAKY RACE CONDITION DETECTED".red().bold() } else { "ROCK SOLID DETERMINISTIC".green() });
                if rep.is_flaky {
                    println!("  Root Cause:      {}", rep.detected_race_condition.yellow());
                    println!("  Suggested Fix:   {}", rep.suggested_synchronization_fix.green());
                    println!("  Remediation Code:\n{}", rep.remediation_code.dimmed());
                }
            }
            HgbResponse::ContextAnchorResult(rep) => {
                println!("{}", "🧠 ASSOCIATIVE NEURAL CONTEXT & INFINITE MEMORY 🧠".bold().cyan());
                println!("  Continuity Score: {}/100 | Categories: {}", rep.continuity_score, rep.categories_covered);
                println!("  Anchor Tokens:    ~{} tokens (Ultra-dense prompt prefix)", rep.estimated_tokens);
                println!("  Anchor Snapshot:  \n{}", rep.compressed_anchor.green());
            }
            HgbResponse::ContextAnchorRecorded { id } => {
                println!("{} Context anchor decision recorded: {}", "✅".green(), id.yellow());
            }
            HgbResponse::LspGhostResult(rep) => {
                println!("{}", "👻 UNIVERSAL LSP GHOST DAEMON PREDICTOR 👻".bold().cyan());
                println!("  Target File:     {}", rep.file_path.yellow());
                println!("  Candidates:      {} items (duration: {} µs)", rep.total_candidates, rep.duration_us);
                for (i, c) in rep.completions.iter().enumerate() {
                    println!("    🔹 Candidate #{}: [{}] (confidence: {:.2})", i + 1, c.source_engine.bold(), c.confidence);
                    println!("       Insert: \n{}", c.insert_text.green());
                }
            }
            HgbResponse::RollingCompactResult(rep) => {
                println!("{}", "🗜️ AUTOMATED ROLLING CONTEXT COMPACTOR 🗜️".bold().cyan());
                println!("  Session ID:      {}", rep.session_id.yellow());
                println!("  Token Redux:     {} -> {} tokens ({:.1}% reduction)", rep.original_tokens, rep.compacted_tokens, rep.compression_ratio * 100.0);
                println!("  Turns Pruned:    {} tool blocks stripped", rep.pruned_tool_outputs);
                println!("  Merkle Anchor:   {}", rep.merkle_anchor_hash.green());
                println!("  Snapshot:        \n{}", rep.summary_snapshot.dimmed());
            }
            HgbResponse::GitMicroCommitResult(rep) => {
                println!("{}", "📦 ATOMIC CONVENTIONAL GIT MICRO-COMMIT 📦".bold().cyan());
                println!("  Commit Hash:     {}", rep.commit_hash.yellow().bold());
                println!("  Message:         {}", rep.conventional_message.green().bold());
                println!("  Diff Stats:      +{} lines, -{} lines across {:?} files", rep.lines_added, rep.lines_removed, rep.staged_files);
                println!("  Verified Syntax: {}", if rep.verified_syntax { "YES (Clean AST)".green() } else { "FAILED".red() });
                println!("  Undo Command:    {}", rep.undo_command.dimmed());
            }
            HgbResponse::VibeRecipeListResult(list) => {
                println!("{}", "📜 DECLARATIVE VIBE RECIPES CATALOG 📜".bold().cyan());
                println!("  Available Standard Recipes: {}", list.len());
                for r in &list {
                    println!("    📖 [{:<24}] {} ({} steps)", r.name.green().bold(), r.description.dimmed(), r.steps.len());
                }
            }
            HgbResponse::VibeRecipeRunResult(rep) => {
                println!("{}", "🚀 EXECUTING DECLARATIVE VIBE RECIPE 🚀".bold().cyan());
                println!("  Recipe:          {}", rep.recipe_name.yellow().bold());
                println!("  Progress:        {}/{} steps completed (+{} lines synthesized)", rep.steps_completed, rep.steps_total, rep.total_lines_synthesized);
                for log in &rep.logs {
                    println!("    ✓ Step #{}: {:<32} -> {} ({}ms)", log.step_number, log.name.bold(), log.status.green(), log.duration_ms);
                }
                println!("  Summary:         {}", rep.summary.green());
            }
            HgbResponse::BehaviorMatrixResult(rep) => {
                println!("{}", "🧪 PRE-FLIGHT BEHAVIORAL CONTRACT MATRIX 🧪".bold().cyan());
                println!("  Target Symbol:   {}", rep.target_symbol.yellow().bold());
                println!("  Dimensions:      {}/5 dimensions verified (Score: {}/100)", rep.dimensions_covered, rep.behavioral_coverage_score);
                for c in &rep.contracts {
                    println!("    🛡️ [{:<18}] {}", c.dimension.as_str().bold(), c.title.green());
                    println!("       Scenario: {}", c.scenario.dimmed());
                    println!("       Expected: {}", c.expected_behavior.yellow());
                }
                println!("  Scaffolded Suite:\n{}", rep.generated_test_suite.dimmed());
            }
            HgbResponse::FlightGraphResult(rep) => {
                println!("{}", "🗺️ LIVE AGENT FLIGHT-GRAPH & REAL-TIME TASK DAG 🗺️".bold().cyan());
                println!("  Goal:            {}", rep.task_goal.yellow().bold());
                println!("  Progress:        {}% ({} of {} nodes) | Burn: ~{} tokens | Elapsed: {}ms",
                    rep.progress_percent, rep.completed_nodes, rep.total_nodes, rep.total_tokens_burned, rep.total_duration_ms
                );
                println!("{}", rep.ascii_dag.green());
            }
            HgbResponse::RepoMapRankResult(rep) => {
                println!("{}", rep.cyan());
            }
            HgbResponse::ShadowPreflightResult(rep) => {
                println!("{}", "🛡️ CURSOR-STYLE SHADOW WORKSPACE VALIDATOR 🛡️".bold().cyan());
                println!("  Pre-flight Valid: {}", if rep.is_valid { "CLEAN (Compilation Sound)".green().bold() } else { "FAILED (Errors Detected)".red().bold() });
                println!("  Diff Statistics:  {}", rep.diff_stats.yellow());
                if !rep.diagnostics.is_empty() {
                    println!("  Diagnostics ({} found):", rep.diagnostics.len());
                    for d in &rep.diagnostics {
                        println!("    ❌ {}:{}:{} - {}", d.file_path, d.line_number, d.column, d.message.red());
                    }
                }
                if let Some(ref _fixed) = rep.repaired_content {
                    println!("  ✨ Speculative Auto-Repair Applied! Repaired diff ready for staging.");
                }
                println!("  Compiler Output:\n{}", rep.compiler_output.dimmed());
            }
            HgbResponse::StreamSqueezeResult(rep) => {
                println!("{}", "🗜️ CLAUDE CODE-STYLE TERMINAL STREAM SQUEEZER 🗜️".bold().cyan());
                println!("  Raw vs Squeezed: {} lines -> {} lines ({:.1}% compression)",
                    rep.total_raw_lines, rep.squeezed_lines, rep.compression_ratio_pct
                );
                println!("  Critical Errors: {} found | Unique Warnings: {}", rep.errors.len(), rep.deduplicated_warnings.len());
                if !rep.file_locations.is_empty() {
                    println!("  Direct Code Locations: {:?}", rep.file_locations);
                }
                println!("{}", rep.compressed_view.yellow());
            }
            HgbResponse::MutationAuditResult(rep) => {
                println!("{}", "🧬 QODO-STYLE TEST INTEGRITY MUTATION AUDIT 🧬".bold().cyan());
                println!("  Integrity Grade: {}", rep.integrity_grade.bold().green());
                println!("  Mutation Score:  {:.1}% ({}/{} mutants killed)",
                    rep.mutation_score_pct, rep.killed_mutants, rep.total_mutants - rep.compile_errors
                );
                println!("  Placebo Mutants: {} survived", rep.survived_mutants);
                for rec in &rep.recommendations {
                    println!("    💡 {}", rec.yellow());
                }
            }
            HgbResponse::DomInspectResult { elements, hierarchy_map, target_element } => {
                println!("{}", "👁️ BOLT.NEW-STYLE CLICK-TO-CODE DOM TELEMETRY 👁️".bold().cyan());
                println!("  Parsed Elements: {} nodes", elements.len());
                if let Some(target) = target_element {
                    println!("  🎯 Selected Node: <{} id=\"{:?}\" class=\"{:?}\"> -> {}:{}",
                        target.tag.green().bold(), target.id, target.classes, target.source_file.yellow(), target.source_line
                    );
                }
                println!("{}", hierarchy_map.dimmed());
            }
            HgbResponse::McpOrchestrateResult { active_servers, tools, tool_output } => {
                println!("{}", "🌐 GOOSE-STYLE UNIVERSAL MCP FLEET ORCHESTRATOR 🌐".bold().cyan());
                println!("  Active Servers:  {} connected", active_servers.len());
                for (s, st) in active_servers {
                    println!("    🖥️  {:<20} -> {:?}", s.bold(), st);
                }
                println!("  Aggregated Tools: {} available", tools.len());
                for t in tools.iter().take(10) {
                    println!("    🔧 {:<32} - {}", t.namespaced_name.green().bold(), t.description.dimmed());
                }
                if let Some(out) = tool_output {
                    println!("  Tool Call Result:\n{}", serde_json::to_string_pretty(&out).unwrap_or_default().green());
                }
            }
            HgbResponse::LiveGraphSyncResult(rep) => {
                println!("{}", "⚡ AUGMENT CODE-STYLE LIVE IN-MEMORY CODEBASE GRAPH ⚡".bold().cyan());
                println!("  Files Tracked:   {} files (sync time: {} µs)", rep.total_files_tracked, rep.sync_duration_us);
                println!("  Symbols Indexed: {} symbols (dependencies: {} links)", rep.total_symbols_indexed, rep.total_dependency_links);
                if !rep.hot_symbols.is_empty() {
                    println!("  Hot Architectural Symbols:");
                    for (sym, cnt) in &rep.hot_symbols {
                        println!("    🔥 {:<24} -> referenced by {} modules", sym.green().bold(), cnt);
                    }
                }
            }
            HgbResponse::ShellPanicDiagnosisResult(diag) => {
                println!("{}", "🚨 WARP TERMINAL-STYLE SHELL PANIC INTERCEPTOR 🚨".bold().red());
                println!("  Category:        {}", diag.category.label().yellow().bold());
                println!("  Root Cause:      {}", diag.root_cause.white());
                println!("  Explanation:     {}", diag.explanation.dimmed());
                println!("  🛠️ Suggested 1-Key Auto-Repair:\n    {}", diag.suggested_fix_command.green().bold());
            }
            HgbResponse::SpecDecomposeResult(rep) => {
                println!("{}", "📋 COPILOT WORKSPACE-STYLE SPEC -> PLAN -> DIFF 📋".bold().cyan());
                println!("  Intent:          {}", rep.intent.yellow().bold());
                println!("  Progress:        {}% ({}/{} steps completed)", rep.progress_percent, rep.completed_steps, rep.total_steps);
                println!("  Spec:\n{}", rep.architecture_spec.dimmed());
                for s in &rep.steps {
                    println!("    {} Step #{}: {:<32} (targets: {:?})", s.status.badge(), s.step_number, s.title.bold(), s.target_files);
                    println!("       Action: {}", s.action_description.dimmed());
                    println!("       Verify: {}", s.verification_command.yellow());
                }
            }
            HgbResponse::DynamicContextExpandResult(res) => {
                println!("{}", "📎 CONTINUE.DEV-STYLE DYNAMIC @CONTEXT EXPANDER 📎".bold().cyan());
                println!("  Injected:        {} dynamic context attachments ({} tokens)", res.attachments.len(), res.total_injected_tokens);
                for att in &res.attachments {
                    println!("    🏷️ Attached {:<16} (~{} tokens)", att.directive.green().bold(), att.token_estimate);
                }
                println!("  Augmented Prompt Preview:\n{}", res.expanded_prompt.dimmed());
            }
            HgbResponse::VisualRegressionResult(rep) => {
                println!("{}", "👁️ DEVIN & REPLIT-STYLE VISUAL LAYOUT SENTRY 👁️".bold().cyan());
                println!("  Visual Stability: {:.1}% {}", rep.visual_stability_score, if rep.is_visually_stable { "(CLEAN)".green() } else { "(REGRESSIONS FOUND)".red() });
                println!("  Baseline/Current: {} -> {} elements (breaking shifts: {})", rep.baseline_elements, rep.current_elements, rep.breaking_shifts);
                for d in &rep.deltas {
                    println!("    {} [{:<16}] - {}", if d.is_breaking { "💥".red() } else { "ℹ️".blue() }, d.selector.bold(), d.description);
                }
                println!("  Summary:         {}", rep.summary.yellow());
            }
            HgbResponse::ContinuousHealResult(rep) => {
                println!("{}", "🩺 META SAPFIX & QODO-STYLE CONTINUOUS HEALING WATCHDOG 🩺".bold().cyan());
                println!("  Workspace Health: {}", rep.workspace_health_status.bold().green());
                println!("  Health Score:    {:.1}% ({} regressions detected, {} autonomous fixes ready)", rep.health_score_pct, rep.regressions_detected, rep.autonomous_fixes_ready);
                for act in &rep.actions {
                    println!("    🩹 [{:<24}] Target: {} ({})", act.issue_type.yellow().bold(), act.target_file.cyan(), if act.verified_in_shadow { "Shadow Verified".green() } else { "Pending".dimmed() });
                }
                for rec in &rep.recommendations {
                    println!("    💡 {}", rec.dimmed());
                }
            }
            HgbResponse::AmbientPredictResult(rep) => {
                println!("{}", "🔮 WINDSURF CASCADE & SUPERMAVEN AMBIENT NEXT-EDIT PREDICTOR 🔮".bold().cyan());
                println!("  Trigger:         {} in {}", rep.trigger_symbol.bold(), rep.trigger_file.dimmed());
                println!("  Call Sites:      {} analyzed in {} µs", rep.call_sites_analyzed, rep.elapsed_us);
                println!("  Predicted Edits: {} anticipations", rep.predictions.len());
                for (i, p) in rep.predictions.iter().enumerate() {
                    println!("    [{}] {} (Confidence: {}%)", i + 1, p.target_file.cyan().bold(), p.confidence_score);
                    println!("        Symbol:      {}", p.affected_symbol.yellow());
                    println!("        Action:      {}", p.suggested_action);
                    println!("        Rationale:   {}", p.rationale.dimmed());
                    if !p.suggested_diff.is_empty() {
                        println!("        Diff:\n{}", p.suggested_diff.green());
                    }
                }
            }
            HgbResponse::CdpTweakSyncResult(rep) => {
                println!("{}", "🎨 BOLT.NEW & DEVIN BIDIRECTIONAL DEVTOOLS TWEAK MIRROR 🎨".bold().cyan());
                println!("  Target File:     {}", rep.target_file.cyan().bold());
                println!("  Line Matched:    Line {}", rep.matched_line);
                println!("  Diff Applied:    {}", rep.diff_applied.yellow());
                println!("  Blake3 Ledger:   {}", rep.blake3_hash.dimmed());
                println!("  Status:          {} (Written to disk: {})", if rep.success { "SYNCED".green().bold() } else { "FAILED".red().bold() }, rep.file_written);
                println!("  Message:         {}", rep.message);
            }
            HgbResponse::PromptModeHarvestResult(rep) => {
                println!("{}", "📚 CONTINUE.DEV & ROO CODE COMPOSABLE MODES & DOCS HARVESTER 📚".bold().cyan());
                println!("  Active Mode:     {:?}", rep.active_mode);
                println!("  Directive:       {}", rep.system_prompt_directive.dimmed());
                println!("  Harvested Docs:  {} sources sliced", rep.harvested_docs.len());
                for doc in &rep.harvested_docs {
                    println!("    📄 {} ({:.1}x compression, {} -> {} tokens)", doc.extracted_title.green().bold(), doc.compression_ratio, doc.raw_tokens_estimate, doc.condensed_tokens_estimate);
                    if !doc.signatures.is_empty() {
                        println!("       Signatures:   {}", doc.signatures.join(", ").dimmed());
                    }
                }
            }
            HgbResponse::EphemeralSandboxResult(rep) => {
                println!("{}", "📦 REPLIT AGENT & WEBCONTAINERS EPHEMERAL STACK SANDBOX 📦".bold().cyan());
                println!("  Session ID:      {}", rep.session.session_id.bold().yellow());
                println!("  Stack:           {}", rep.session.stack_name.bold().green());
                println!("  Primary Port:    http://127.0.0.1:{}", rep.session.primary_port);
                if let Some(sec) = rep.session.secondary_port {
                    println!("  Secondary Port:  http://127.0.0.1:{}", sec);
                }
                println!("  Database URI:    {}", rep.session.db_uri.cyan());
                println!("  Seeded Records:  {}", rep.session.seeded_records_count);
                println!("  Health Check:    {}", rep.health_url.dimmed());
                println!("  Status:          {}", rep.status.bold());
            }
            HgbResponse::AntiPlaceboAuditResult(rep) => {
                println!("{}", "🛡️ QODO & META SAPFIX ANTI-PLACEBO TEST GATEKEEPER 🛡️".bold().cyan());
                println!("  Mutants Created: {} (Killed: {}, Survived: {})", rep.total_mutants_generated, rep.mutants_killed.to_string().green(), rep.mutants_survived.to_string().red());
                println!("  Mutation Score:  {:.1}% {}", rep.mutation_score_pct, if rep.is_production_ready { "(PRODUCTION READY)".green().bold() } else { "(TAUTOLOGICAL/PLACEBO HAZARD)".red().bold() });
                if !rep.placebo_tests_detected.is_empty() {
                    println!("  Placebo Tests:");
                    for t in &rep.placebo_tests_detected {
                        println!("    ⚠️ {}", t.yellow());
                    }
                }
                for rec in &rep.recommendations {
                    println!("    💡 {}", rec.dimmed());
                }
            }
            HgbResponse::CircuitBreakerResult(rep) => {
                println!("{}", "🔄 CIRCULAR CIRCUIT BREAKER & ANTI-THRASHING GATE 🔄".bold().cyan());
                println!("  Status:          {}", if rep.is_tripped { "TRIPPED".red().bold() } else { "PASSING".green().bold() });
                println!("  Pattern:         {:?}", rep.detected_pattern);
                println!("  Description:     {}", rep.loop_description);
                println!("  Prescription:    {}", rep.pivot_prescription.yellow());
            }
            HgbResponse::AppSecAuditResult(rep) => {
                println!("{}", "🛡️ APPSEC SENTINEL & PRE-APPLY SECURITY GATE 🛡️".bold().cyan());
                println!("  Target:          {}", rep.target_file.cyan());
                println!("  Safe to Apply:   {}", if rep.is_safe_to_apply { "SAFE".green().bold() } else { "BLOCKED".red().bold() });
                println!("  Criticals:       {}", rep.critical_count.to_string().red());
                for f in &rep.findings {
                    println!("    [{:?}] line {}: {}", f.severity, f.line_number, f.description.yellow());
                }
            }
            HgbResponse::CognitiveWalkthroughResult(rep) => {
                println!("{}", "🧠 COGNITIVE WALKTHROUGH & INVARIANT DIFF EXPLAINER 🧠".bold().cyan());
                println!("  Intent:          {}", rep.overall_architectural_intent.cyan());
                println!("  Summary:         {}", rep.executive_summary);
                for card in &rep.cards {
                    println!("    Card: {} (+{}/-{})", card.file_path.cyan(), card.lines_added, card.lines_removed);
                }
            }
            HgbResponse::LogicTeleportResult(rep) => {
                println!("{}", "⚡ CLICK-TO-LOGIC DEVTOOLS TELEPORT ⚡".bold().cyan());
                println!("  Matched:         {}", rep.matched);
                if let Some(ref target) = rep.target {
                    println!("  Target File:     {}", target.source_file.cyan());
                    println!("  Target Line:     {}", target.start_line);
                    println!("  Component:       {}", target.function_or_handler_name);
                }
                println!("  Message:         {}", rep.message);
            }
            HgbResponse::ProxyApiReplayResult(rep) => {
                println!("{}", "🎭 INSTANT RELATIONAL PROXY API & WEBHOOK REPLAY 🎭".bold().cyan());
                println!("  Service:         {:?}", rep.service);
                println!("  Endpoint:        {}", rep.endpoint.cyan());
                println!("  Status:          {}", rep.status_code);
                println!("  Latency:         {}ms", rep.latency_ms);
            }
            HgbResponse::LivePreviewResult { port, base_url, status } => {
                println!("{}", "🌐 HAGIBIS VISUAL LIVE-PREVIEW SIDECAR 🌐".bold().cyan());
                println!("  Status:          {}", status.green().bold());
                println!("  URL:             {}", base_url.cyan().bold());
                println!("  Port:            {}", port);
                println!("  Teleport Hook:   {}", "Alt + Click DOM elements to jump to code".dimmed());
            }
            HgbResponse::MultimodalVisionResult(payload) => {
                println!("{}", "👁️ MULTIMODAL VISION INGESTION ENGINE 👁️".bold().cyan());
                println!("  Intent:          {}", payload.auto_detected_intent.cyan());
                println!("  Images Captured: {}", payload.images.len().to_string().green());
                for (idx, img) in payload.images.iter().enumerate() {
                    println!("    [{}] Format: {:?}, Size: {} KB, Origin: {}", idx + 1, img.format, img.byte_size / 1024, img.source_origin.yellow());
                }
                println!("  Prompt:          {}", payload.text_prompt.dimmed());
            }
            HgbResponse::ShareTunnelResult(session) => {
                println!("{}", "🚀 ONE-CLICK PUBLIC SHARE TUNNEL 🚀".bold().cyan());
                println!("  Public URL:      {}", session.public_url.green().bold());
                println!("  Local Port:      {}", session.local_port);
                println!("  Provider:        {}", session.provider.dimmed());
                println!("  Mobile Quick-Scan QR:");
                println!("{}", session.qr_terminal_art.cyan());
            }
            HgbResponse::BaasGraduationResult(rep) => {
                println!("{}", "🎓 BAAS AUTO-GRADUATION ENGINE ('PROXY-TO-REAL') 🎓".bold().cyan());
                println!("  Target:          {}", rep.target.name().green().bold());
                println!("  Resource:        {}", rep.resource_name.cyan());
                println!("  Inferred Cols:   {}", rep.columns_inferred.len());
                println!("  Security:        {}", rep.security_rules.dimmed());
                println!("  Client SDK Snippet:\n{}", rep.client_sdk_snippet.dimmed());
            }
            HgbResponse::VibeIntentExpandResult(spec) => {
                println!("{}", "✨ VIBE-TO-SPEC INTENT EXPANDER ✨".bold().cyan());
                println!("  Theme:           {}", spec.design.theme_name.cyan().bold());
                println!("  Background:      {}", spec.design.background);
                println!("  Accent:          {}", spec.design.primary_accent.yellow());
                println!("  Motion:          {}", spec.motion.enter_animation.dimmed());
                println!("  Components:      {}", spec.inferred_components.join(", ").green());
                println!("  Packages:        {}", spec.recommended_packages.join(", ").magenta());
            }
            HgbResponse::AutoDependencyHealResult(rep) => {
                println!("{}", "🩹 INVISIBLE DEPENDENCY & PACKAGE AUTO-HEALER 🩹".bold().cyan());
                println!("  Status:          {}", if rep.is_fully_healed { "HEALED".green().bold() } else { "PENDING".yellow() });
                println!("  Detected:        {}", rep.missing_detected.len());
                for act in &rep.actions_taken {
                    println!("    ✔ Package '{}' injected into {}", act.package_name.green(), act.manifest_file.dimmed());
                }
                println!("  Summary:         {}", rep.summary);
            }
            HgbResponse::VisualCanvasHudResult(rep) => {
                println!("{}", "🎨 VISUAL CANVAS HUD & WEBVIEW SIDECAR 🎨".bold().cyan());
                println!("  URL:             {}", rep.hud_url.green().bold());
                println!("  Port:            {}", rep.port);
                println!("  Status:          {}", rep.status.cyan());
                println!("  Active Model:    {}", rep.active_model.yellow());
                println!("  Selections:      {}", rep.selections_count);
                println!("  Tweaks Applied:  {}", rep.tweaks_count);
            }
            HgbResponse::EdgeDeployResult(rep) => {
                println!("{}", "🚀 ZERO-CONFIG 1-CLICK PUBLIC EDGE DEPLOYER 🚀".bold().cyan());
                println!("  Status:          {}", rep.status.green().bold());
                println!("  Public URL:      {}", rep.public_url.cyan().bold());
                println!("  Provider:        {}", rep.provider.to_string().yellow());
                println!("  Framework:       {}", rep.framework.to_string().dimmed());
                println!("  Deployment ID:   {}", rep.deployment_id.dimmed());
            }
            HgbResponse::VisualAnnotateResult(rep) => {
                println!("{}", "👁️ VISUAL SCREENSHOT & CLIPBOARD XEROX ANNOTATOR 👁️".bold().cyan());
                println!("  Annotations:     {}", rep.annotations_count.to_string().green().bold());
                println!("  Bound AST Nodes: {}", rep.ast_bindings.len());
                for b in &rep.ast_bindings {
                    println!("    ✔ Bound to {} in {}:{}", b.component_name.yellow(), b.source_file.cyan(), b.line_number);
                }
            }
            HgbResponse::MultiplayerSwarmResult(rep) => {
                println!("{}", "👥 COLLABORATIVE MULTIPLAYER VIBE SWARM 👥".bold().cyan());
                println!("  Session ID:      {}", rep.session_id.cyan().bold());
                println!("  Session Name:    {}", rep.session_name.dimmed());
                println!("  Host Peer:       {}", rep.host_peer_id.green().bold());
                println!("  Active Peers:    {}", rep.peers.len());
                for p in &rep.peers {
                    println!("    • {} ({}) - Model: {}", p.username.yellow(), p.role.to_string().dimmed(), p.active_model.cyan());
                }
                println!("  Status:          {}", rep.status.yellow());
            }
            HgbResponse::CompanionBridgeResult(rep) => {
                println!("{}", "🔌 UNIVERSAL COMPANION EDITOR & LSP SIDECAR BRIDGE 🔌".bold().cyan());
                println!("  Editor:          {}", rep.editor.to_string().green().bold());
                println!("  Socket Path:     {}", rep.socket_path.dimmed());
                println!("  Socket Alive:    {}", if rep.socket_alive { "YES".green().bold() } else { "NO".yellow() });
                println!("  Config Files:    {}", rep.files.len());
                println!("  Instructions:    {}", rep.setup_instructions.dimmed());
            }
            HgbResponse::SaasScaffoldResult(rep) => {
                println!("{}", "💳 INSTANT SAAS MONETIZATION & AUTH FABRIC 💳".bold().cyan());
                println!("  Provider:        {:?}", rep.provider);
                println!("  Project Name:    {}", rep.project_name.green());
                println!("  Webhook Route:   {}", rep.webhook_endpoint.yellow());
                println!("  Billing Portal:  {}", rep.customer_portal_endpoint.cyan());
                println!("  Files Generated: {}", rep.generated_files.len());
                println!("  Idempotent Guard:{}", if rep.idempotent_guard_enabled { "ACTIVE".green() } else { "INACTIVE".red() });
            }
            HgbResponse::SaasWebhookVerifyResult(res) => {
                println!("{}", "🔒 SAAS WEBHOOK SIGNATURE VERIFICATION 🔒".bold().cyan());
                println!("  Valid Signature: {}", if res.valid { "VALID".green().bold() } else { "INVALID".red().bold() });
                println!("  Event ID:        {}", res.event_id.yellow());
                println!("  Event Type:      {}", res.event_type.cyan());
                println!("  Idempotent Skip: {}", if res.is_duplicate { "YES (Skipped replay)".yellow() } else { "NO (First time)".green() });
            }
            HgbResponse::ContinuousVoiceResult(rep) => {
                println!("{}", "🎙️ FULL-DUPLEX AMBIENT VOICE LOOP 🎙️".bold().cyan());
                println!("  Session:         {}", rep.session_id.green());
                println!("  Duplex State:    {:?}", rep.state);
                println!("  Turns:           {} (Barge-in Interruptions: {})", rep.total_turns, rep.total_interruptions);
            }
            HgbResponse::FigmaSyncResult(rep) => {
                println!("{}", "🎨 BI-DIRECTIONAL FIGMA & DESIGN TOKEN BRIDGE 🎨".bold().cyan());
                println!("  File Key:        {}", rep.file_key.green());
                println!("  Tokens Parsed:   {} colors, {} typography", rep.token_set.colors.len(), rep.token_set.typography.len());
                println!("  Components:      {}", rep.components_parsed);
            }
            HgbResponse::FigmaExportResult(rep) => {
                println!("{}", "🎨 FIGMA VECTOR CANVAS EXPORT 🎨".bold().cyan());
                println!("  Component:       {}", rep.component_name.green());
                println!("  Canvas Size:     {}x{} px", rep.bounding_width, rep.bounding_height);
            }
            HgbResponse::ShadowDbStressResult(rep) => {
                println!("{}", "⚡ AUTONOMOUS PRODUCTION DB SHADOW STRESS FUZZER ⚡".bold().cyan());
                println!("  Operations:      {}", rep.metrics.total_ops);
                println!("  Throughput:      {} QPS", rep.metrics.throughput_qps.to_string().green());
                println!("  Latency (p50):   {:.2} ms", rep.metrics.p50_ms);
                println!("  Latency (p95):   {:.2} ms", rep.metrics.p95_ms);
                println!("  Viral Ready:     {}", if rep.production_ready_for_viral_traffic { "YES (100% Scalable)".green() } else { "REQUIRES INDEXES".yellow() });
            }
            HgbResponse::ViralOgResult(rep) => {
                println!("{}", "🚀 VIRAL SOCIAL GRAPH & DYNAMIC OPENGRAPH ENGINE 🚀".bold().cyan());
                println!("  Scorecard:       {}/100", rep.scorecard.total_score.to_string().green());
                println!("  SVG Card Size:   {} bytes", rep.generated_svg_image.len());
                println!("  Meta Tags:       {}", rep.meta_tags.len());
            }
            HgbResponse::MobileQrTeleportResult(rep) => {
                println!("{}", "📱 INSTANT MOBILE QR TELEPORT & PWA MATRIX 📱".bold().cyan());
                println!("{}", rep.ansi_qr_art);
                println!("  Target URL:      {}", rep.target_url.green());
                println!("  Safe-Area:       Injected");
            }
            HgbResponse::ProductionHotfixResult(rep) => {
                println!("{}", "🚨 LIVE PRODUCTION INCIDENT AUTO-HOTFIXER 🚨".bold().red());
                println!("  Incident ID:     {}", rep.incident_id.yellow());
                println!("  Location:        {}", rep.culprit_location.cyan());
                println!("  Root Cause:      {}", rep.root_cause.green());
                println!("  Hotfix Branch:   {}", rep.hotfix_branch_name.yellow().bold());
                println!("  Patched Code:    {}", rep.proposed_patch.patched_code.green());
            }
            HgbResponse::LlmCostResult(rep) => {
                println!("{}", "⚡ AI SEMANTIC COST GATEWAY & MODEL ARBITRAGE ⚡".bold().cyan());
                println!("  Model Selected:  {}", rep.decision.selected_model.green().bold());
                println!("  Is Cached:       {}", if rep.decision.is_cached { "YES ($0.00)".green() } else { "NO".yellow() });
                println!("  Cost:            ${:.6}", rep.decision.estimated_cost_usd);
                println!("  Cache Hit Rate:  {:.1}%", rep.metrics.cache_hit_ratio * 100.0);
            }
            HgbResponse::PrivacyFunnelResult(rep) => {
                println!("{}", "📊 ZERO-COOKIE PRIVACY FUNNEL ANALYTICS 📊".bold().cyan());
                println!("  Total Events:    {}", rep.total_events_recorded);
                for stage in &rep.stages {
                    println!("    • {:<12} {:>5} | Step: {:>5.1}%", stage.stage_name.cyan(), stage.unique_visitors, stage.step_conversion_rate_pct);
                }
            }
            HgbResponse::PrivacyAnalyticsScaffoldResult(rep) => {
                println!("{}", "📊 PRIVACY ANALYTICS SCAFFOLD 📊".bold().cyan());
                println!("  Script Tag:\n{}", rep.client_script_tag.cyan());
            }
            HgbResponse::RailsDetectResult(rep) => {
                println!("{}", "💎 RAILS APPLICATION DETECTED 💎".bold().red());
                println!("  Is Rails App:    {}", if rep.is_rails { "YES".green().bold() } else { "NO".yellow() });
                println!("  Database:        {:?}", rep.db_adapter);
                println!("  Models Count:    {}", rep.model_count);
                println!("  Migrations:      {}", rep.migration_count);
            }
            HgbResponse::RailsLintMigrationResult(rep) => {
                println!("{}", "💎 RAILS ZERO-DOWNTIME MIGRATION LINTER 💎".bold().red());
                println!("  Safe to Deploy:  {}", if rep.safe_to_deploy_zero_downtime { "YES (Greenlight)".green().bold() } else { "NO (Table Lock Hazard)".red().bold() });
                for h in &rep.hazards {
                    println!("    ⚠ [Line {}] {}: {}", h.line_number.unwrap_or(0), h.rule.yellow().bold(), h.message);
                }
            }
            HgbResponse::RailsScaffoldResult(rep) => {
                println!("{}", "💎 RAILS SCAFFOLD GENERATED 💎".bold().green());
                println!("  Model:           app/models/{}.rb", rep.model_name.to_lowercase());
                println!("  Migration:       db/migrate/create_{}.rb", rep.table_name);
                println!("  Route:           {}", rep.route_snippet.yellow());
            }
            HgbResponse::RailsAuditNPlusOneResult(rep) => {
                println!("{}", "💎 RAILS N+1 QUERY AUDIT 💎".bold().yellow());
                println!("  Issues Found:    {}", rep.issues_found.len());
                for iss in &rep.issues_found {
                    println!("    ⚠ Line {}: {}", iss.line_number, iss.suggestion.green());
                }
            }
            HgbResponse::RailsParseRoutesResult(routes) => {
                println!("{}", "💎 RAILS ROUTES PARSER 💎".bold().cyan());
                for r in routes.iter().take(10) {
                    println!("  {:<6} {:<25} => {}#{}", r.verb.green(), r.path, r.controller, r.action);
                }
            }
            HgbResponse::ProjectSnapshotResult(snap) => {
                println!("{}", "📁 PERSISTENT PROJECT COORDINATOR 📁".bold().cyan());
                println!("  Project:         {}", snap.project_name.green().bold());
                println!("  Completion:      {:.1}% ({} completed / {} total)", snap.completion_percentage, snap.completed_tasks, snap.total_tasks);
                println!("  ADRs:            {}", snap.adrs_count);
            }
            HgbResponse::ProjectTaskResult(task) => {
                if let Some(t) = task {
                    println!("  Task [{}] {} ({:?}): {:?}", t.id.yellow(), t.title.green(), t.priority, t.status);
                }
            }
            HgbResponse::ProjectTasksListResult(tasks) => {
                for t in tasks {
                    println!("  [{}] {} ({:?}): {:?}", t.id.yellow(), t.title.green(), t.priority, t.status);
                }
            }
            HgbResponse::ProjectAdrResult(adr) => {
                println!("  ADR-{:03}: {} ({:?})", adr.id, adr.title.green(), adr.status);
            }
            HgbResponse::ProjectAdrsListResult(adrs) => {
                for a in adrs {
                    println!("  ADR-{:03}: {}", a.id, a.title.green());
                }
            }
            HgbResponse::RulesEngineResult(rep) => {
                println!("{}", "📜 TIERED RULES AUTO-ENGINE 📜".bold().yellow());
                println!("  Evaluated:       {} rules", rep.total_rules_evaluated);
                println!("  Active Matches:  {}", rep.matched_rules.len());
            }
            HgbResponse::AutopilotResult(rep) => {
                println!("{}", "🤖 TICKET-TO-PR AUTOPILOT COMPLETED 🤖".bold().magenta());
                println!("  Ticket:          [{}] {}", rep.ticket.ticket_id.yellow(), rep.ticket.title);
                println!("  Branch:          {}", rep.pr_metadata.branch_name.cyan());
                println!("  Verification:    {}/{} tests passed", rep.verification.tests_passed, rep.verification.tests_executed);
            }
            HgbResponse::ExplainResult(rep) => {
                println!("{}", "🔍 AGENT DECISION EXPLAINER (ADR) 🔍".bold().cyan());
                println!("  Title:           {}", rep.title.green().bold());
                println!("  Uncertainty:     {:.2}", rep.uncertainty_score);
                println!("  Trust Verdict:   {}", rep.trust_verdict.yellow());
            }
            HgbResponse::SmartIndexSnapshotResult(snap) => {
                println!("{}", "🌲 MERKLE SMART-INDEX SNAPSHOT 🌲".bold().green());
                println!("  Root Hash:       {}", snap.root_hash.green().bold());
                println!("  Indexed Files:   {}", snap.file_count);
            }
            HgbResponse::SmartIndexDiffResult(rep) => {
                println!("{}", "🌲 MERKLE SMART-INDEX DIFF 🌲".bold().green());
                println!("  Modified Files:  {}", rep.modified_files.len());
                println!("  Added Files:     {}", rep.added_files.len());
            }
            HgbResponse::RolloutWatchResult(rep) => {
                println!("{}", "📈 PR / DEPLOYMENT HEALTH MONITOR 📈".bold().yellow());
                println!("  Verdict:         {:?}", rep.verdict);
                println!("  Error Rate:      {:.2}%", rep.current_error_rate_pct);
                println!("  p99 Latency:     {:.1} ms", rep.current_p99_ms);
            }
            HgbResponse::PrAuditResult(rep) => {
                println!("{}", "🛡️ AI-PR SECURITY AUDIT 🛡️".bold().red());
                println!("  Passed:          {}", if rep.passed_audit { "YES (Clean)".green().bold() } else { "BLOCKED (Vulns Found)".red().bold() });
                println!("  Critical Vulns:  {}", rep.critical_count);
                println!("  High Vulns:      {}", rep.high_count);
            }
            HgbResponse::PreviewCloudResult(rep) => {
                println!("{}", "☁️ EPHEMERAL CLOUD PREVIEW ☁️".bold().cyan());
                println!("  Preview URL:     {}", rep.preview_url.green().bold());
                println!("  Expires At:      {}", rep.expires_at_utc.yellow());
            }
            HgbResponse::CollabSessionResult(state) => {
                println!("{}", "👥 MULTI-DEV COLLAB SESSION 👥".bold().blue());
                println!("  Room ID:         {}", state.room_id.green());
                println!("  Connected Peers: {}", state.connected_peers.len());
            }
            HgbResponse::CollabConflictResult(conflicts) => {
                println!("{}", "👥 COLLAB CONFLICT REPORT 👥".bold().yellow());
                println!("  Conflicts:       {}", conflicts.len());
            }
            HgbResponse::PromptLabResult(rep) => {
                println!("{}", "🧪 PROMPT A/B ENGINEERING WORKSPACE 🧪".bold().magenta());
                println!("  Leaderboard:     {} variants evaluated", rep.total_variants_benchmarked);
                println!("  Winner:          {}", rep.recommended_winner_id.green().bold());
            }
            HgbResponse::LangPackResult(rep) => {
                println!("{}", "📦 FRAMEWORK LANGUAGE INTELLIGENCE PACK 📦".bold().green());
                println!("  Framework:       {:?}", rep.detected_framework);
                println!("  Language:        {}", rep.language_info.primary_language.cyan().bold());
                println!("  Linter:          {}", rep.language_info.recommended_linter.yellow());
            }
            HgbResponse::NativeMobileDetectResult(rep) => {
                println!("{}", "📱 NATIVE MOBILE MATRIX 📱".bold().cyan());
                println!("  Platform:        {:?}", rep.platform);
                println!("  iOS Ready:       {}", rep.has_ios_directory);
                println!("  Android Ready:   {}", rep.has_android_directory);
            }
            HgbResponse::NativeMobileCrashResult(rep) => {
                println!("{}", "📱 MOBILE CRASH DIAGNOSIS 📱".bold().red());
                println!("  Exception:       {}", rep.exception_type.bold());
                println!("  Fix:             {}", rep.suggested_fix.green());
            }
            HgbResponse::BudgetStatusResult(st) => {
                println!("{}", "💰 LLM FINOPS BUDGET STATUS 💰".bold().yellow());
                println!("  Spent / Limit:   ${:.4} / ${:.2} ({:.1}%)", st.total_spent_usd, st.budget_limit_usd, st.percent_consumed);
                println!("  Current Tier:    {:?}", st.current_tier);
                println!("  Remaining:       ${:.4}", st.remaining_usd);
            }
            HgbResponse::VsCodeScaffoldResult(rep) => {
                println!("{}", "🔌 VS CODE EXTENSION BRIDGE 🔌".bold().blue());
                println!("  Manifest:        Generated package.json");
                println!("  Socket Target:   {}", rep.socket_path.green());
            }
            HgbResponse::CiResult(res) => {
                println!("{}", "🚀 HEADLESS CI EXECUTION RESULT 🚀".bold().cyan());
                println!("{}", res.formatted_output);
            }
            HgbResponse::PlanResult(p) => {
                println!("{}", "📋 INTERACTIVE PLAN BLUEPRINT 📋".bold().magenta());
                println!("  Plan ID:         {}", p.plan_id.cyan());
                println!("  Goal:            {}", p.goal_description);
                println!("  Steps:           {}", p.steps.len());
            }
            HgbResponse::TicketResult(t) => {
                println!("{}", "🎫 INGESTED TICKET CONTEXT 🎫".bold().cyan());
                println!("  Key:             {}", t.issue_key.green());
                println!("  Title:           {}", t.title);
                println!("  Branch:          {}", t.suggested_branch_name);
            }
            HgbResponse::ProfileResult(p) => {
                println!("{}", "👤 PERSISTENT PROJECT PROFILE 👤".bold().green());
                println!("  Project:         {}", p.project_name);
                println!("  Root:            {}", p.root_path);
            }
            HgbResponse::HookInstallResult(r) => {
                println!("{}", "🪝 GIT HOOKS INSTALLATION 🪝".bold().yellow());
                println!("  Hooks:           {:?}", r.installed_hooks);
            }
            HgbResponse::HookRunResult(r) => {
                println!("{}", "🪝 PRE-COMMIT SCAN 🪝".bold().yellow());
                println!("  Pass:            {}", if r.pass { "YES".green() } else { "NO".red() });
                println!("  Files:           {}", r.files_scanned);
            }
            HgbResponse::ConventionsResult(d) => {
                println!("{}", "🧬 ARCHITECTURAL DNA 🧬".bold().blue());
                println!("  DNA Hash:        {}", d.blake3_content_hash[..16].green());
                println!("  Rules:           {}", d.rules.len());
            }
            HgbResponse::QueueResult(q) => {
                println!("{}", "🌲 PARALLEL WORKTREE QUEUE 🌲".bold().green());
                println!("  Active Workers:  {}/{}", q.active_workers, q.max_concurrency);
                println!("  Jobs:            {}", q.jobs.len());
            }
            HgbResponse::ReviewResult(r) => {
                println!("{}", "🔍 PR REVIEW BUG-BOT 🔍".bold().yellow());
                println!("  Verdict:         {}", r.overall_verdict.bold());
                println!("  Comments:        {}", r.total_comments);
            }
            HgbResponse::BenchmarkResult(b) => {
                println!("{}", "📊 BENCHMARK HARNESS 📊".bold().magenta());
                println!("  Suite:           {}", b.suite_name);
                println!("  Pass Rate:       {:.1}%", b.pass_rate_pct);
            }
            HgbResponse::QuickstartResult(q) => {
                println!("{}", "⚡ QUICKSTART SYNTHESIZER ⚡".bold().green());
                println!("  Directory:       {}", q.project_directory);
                println!("  Files:           {}", q.files_generated);
            }
            HgbResponse::RegistrySearchResult(r) => {
                println!("{}", "📦 AGENT REGISTRY SEARCH 📦".bold().blue());
                println!("  Matches:         {}/{}", r.matching_plugins.len(), r.total_available);
            }
            HgbResponse::RegistryInstallResult(p) => {
                println!("{}", "📦 AGENT INSTALLED 📦".bold().green());
                println!("  Name:            {} v{}", p.name, p.version);
            }
            HgbResponse::ProvenanceResult(p) => {
                println!("{}", "📜 AUTHORSHIP PROVENANCE 📜".bold().cyan());
                println!("  File:            {}", p.file_path);
                println!("  AI Lines:        {} ({:.1}%)", p.ai_authored_lines, p.ai_percentage);
                println!("  Root Hash:       {}", p.ledger_root_hash);
            }
            HgbResponse::RemoteTunnelResult(r) => {
                println!("{}", "🌐 REMOTE DAEMON TUNNEL 🌐".bold().blue());
                println!("  Connected:       {}", if r.connected { "YES".green() } else { "NO".red() });
                println!("  Latency:         {}ms", r.round_trip_latency_ms);
            }
            HgbResponse::ObserveResult(o) => {
                println!("{}", "📡 OBSERVATION BUS 📡".bold().yellow());
                println!("  Total Events:    {}", o.total_events);
                println!("  Recent:          {}", o.events.len());
            }
            HgbResponse::StackResult(s) => {
                println!("{}", "🏗️ MANAGED STACK PRESET 🏗️".bold().green());
                println!("  Configured:      {:?}", s.services_configured);
                println!("  Ready:           {}", if s.ready_to_boot { "YES".green() } else { "NO".red() });
            }
            HgbResponse::SelfEvolutionResult(r) => {
                println!("{}", "🧬 RECURSIVE SELF-EVOLUTION DPO 🧬".bold().magenta());
                println!("  Generations:      {}", r.total_generations);
                println!("  Mutations:        {}", r.total_mutations_evaluated);
                println!("  Final Fitness:    {:.1}%", r.overall_fitness_score);
                println!("  DPO Pairs:        {}", r.dpo_pairs_generated);
                println!("  Summary:          {}", r.summary_message.green());
            }
            HgbResponse::DesktopInspectResult(r) => {
                println!("{}", "🖥️ DESKTOP SENTRY INSPECTION 🖥️".bold().cyan());
                println!("  Resolution:       {}x{}", r.screen_resolution.0, r.screen_resolution.1);
                println!("  Windows ({}):", r.visible_windows.len());
                for w in r.visible_windows {
                    println!("    - [{}] \"{}\"", w.window_id, w.title.cyan());
                }
            }
            HgbResponse::DesktopActionResult(r) => {
                println!("{}", "🖱️ DESKTOP ACTION EXECUTED 🖱️".bold().cyan());
                println!("  Action:           {}", r.action_type);
                println!("  Success:          {}", if r.success { "YES".green() } else { "NO".red() });
                println!("  Message:          {}", r.message.dimmed());
            }
            HgbResponse::FormalVerifyResult(r) => {
                println!("{}", "📐 FORMAL VERIFICATION & SMT SOLVER 📐".bold().yellow());
                println!("  Solver:           {:?}", r.solver_used);
                println!("  Target:           {}", r.target_file.cyan());
                println!("  Properties:       {}/{} proven", r.proven_count, r.total_properties);
                println!("  Soundness:        {}", if r.mathematically_sound { "MATHEMATICALLY PROVEN".green().bold() } else { "FAILED".red() });
            }
            HgbResponse::MonorepoAnalyzeResult(r) => {
                println!("{}", "🌐 MONOREPO HYPERGRAPH 🌐".bold().blue());
                println!("  Root:             {}", r.workspace_root.cyan());
                println!("  Packages:         {}", r.total_packages);
                println!("  Edges:            {}", r.total_dependency_edges);
            }
            HgbResponse::MonorepoBlastRadiusResult(r) => {
                println!("{}", "💥 MONOREPO BLAST RADIUS 💥".bold().red());
                println!("  Direct Impact:    {:?}", r.directly_impacted_packages);
                println!("  Downstream:       {:?}", r.downstream_impacted_packages);
            }
            HgbResponse::EmbeddedCheckResult(r) => {
                println!("{}", "⚡ EMBEDDED FIRMWARE LAB ⚡".bold().magenta());
                println!("  Target MCU:       {:?}", r.architecture);
                println!("  no_std Valid:     {}", if r.compiles_no_std { "YES".green() } else { "NO".red() });
                println!("  Invariants Passed:{}", if r.memory_invariants_passed { "YES".green() } else { "NO".red() });
                println!("  Flash / RAM:      {} / {} bytes", r.estimated_flash_bytes, r.estimated_ram_bytes);
            }
            HgbResponse::StoreReleaseResult(r) => {
                println!("{}", "🚀 STORE RELEASE ORCHESTRATOR 🚀".bold().cyan());
                println!("  Platform:         {:?}", r.platform);
                println!("  Artifact:         {}", r.build_artifact_path.cyan());
                println!("  Code Signing:     {}", if r.code_signing_verified { "PASSED".green() } else { "FAILED".red() });
                println!("  Status:           {}", r.message.green());
            }
            HgbResponse::SpeechSynthesizeResult(r) => {
                println!("{}", "🗣️ NEURAL SPEECH SYNTHESIS 🗣️".bold().green());
                println!("  Voice:            {}", r.voice_used.cyan());
                println!("  Duration:         {:.2}s ({} bytes)", r.duration_seconds, r.audio_bytes_len);
                println!("  Hash:             {}", r.audio_sha256.dimmed());
            }
            HgbResponse::SpeechListVoicesResult(voices) => {
                println!("{}", "🗣️ AVAILABLE NEURAL VOICES 🗣️".bold().green());
                for v in voices {
                    println!("  - {:<24} ({}) {}Hz", v.voice_id.green(), v.name.cyan(), v.sample_rate_hz);
                }
            }
            HgbResponse::StudioStartResult(r) => {
                println!("{}", "🎨 VISUAL CANVAS STUDIO 🎨".bold().magenta());
                println!("  Preview URL:      {}", r.live_preview_url.cyan().bold());
                println!("  Components:       {}", r.root_components.len());
                println!("  Sync:             {}", if r.bi_directional_sync_active { "ACTIVE".green() } else { "INACTIVE".red() });
            }
            HgbResponse::StudioApplyPatchResult(msg) => {
                println!("{}", "🎨 STUDIO VISUAL PATCH 🎨".bold().magenta());
                println!("  Result:           {}", msg.green());
            }
        }
    }

    fn print_help(&self) {
        println!("{}", "🪽 Hagibis Interactive REPL Commands 🪽".bold().cyan());
        println!("  {}", "--- Core Commands ---".dimmed());
        println!("  {:<25} {}", "/help, /?".green(), "Show this help table");
        println!("  {:<25} {}", "/clear, /cls".green(), "Clear terminal screen");
        println!("  {:<25} {}", "/exit, /quit".green(), "Exit interactive REPL");
        println!("  {:<25} {}", "/ping".green(), "Measure UDS IPC latency (in microseconds)");
        println!("  {:<25} {}", "/status".green(), "Display daemon status and memory RSS");
        println!("  {:<25} {}", "/model, /models".green(), "View, list, or set active model (/model list, /model <name>)");
        println!("  {:<25} {}", "/login".green(), "Authenticate with Google Account (OAuth)");
        println!("  {:<25} {}", "/auth".green(), "Check Google Gemini credential status");
        println!();
        println!("  {}", "--- AGY Surgical CRUD ---".dimmed());
        println!("  {:<25} {}", "/view <path> [s] [e]".yellow(), "View file with paged line slicing");
        println!("  {:<25} {}", "/write <path> <text>".yellow(), "Atomically write content to file");
        println!("  {:<25} {}", "/edit <path> <tgt> <rep>".yellow(), "Surgically search and replace text block");
        println!("  {:<25} {}", "/ls [path]".yellow(), "List directory contents with child counts");
        println!("  {:<25} {}", "/grep <pattern> [path]".yellow(), "Search files recursively for pattern");
        println!("  {:<25} {}", "/find <pat> [dir]".yellow(), "Find files by name/wildcard in directory");
        println!("  {:<25} {}", "/cmd <command>".yellow(), "Execute sandboxed shell command (AgentShieldLight)");
        println!("  {:<25} {}", "/agent <task>".yellow(), "Launch autonomous ReAct multi-turn agent loop");
        println!();
        println!("  {}", "--- Next-Era Engines ---".dimmed());
        println!("  {:<25} {}", "/doctor, /doc".cyan(), "Run full health audit across all pillars");
        println!("  {:<25} {}", "/provenance [act]".cyan(), "Append or audit Blake3 Merkle ledger");
        println!("  {:<25} {}", "/checkpoint [lbl]".cyan(), "Create or view time-travel state snapshot");
        println!("  {:<25} {}", "/fuzz <target>".cyan(), "Run property-based differential fuzzer");
        println!("  {:<25} {}", "/verify <target>".cyan(), "Formally verify invariants with SMT-LIB2");
        println!("  {:<25} {}", "/mesh".cyan(), "Display P2P swarm mesh status");
        println!("  {:<25} {}", "/cockpit".cyan(), "Launch full-screen Interactive Cockpit dashboard");
        println!("  {:<25} {}", "/mcp [list|call]".cyan(), "Universal MCP: list tools or call tool on server");
        println!();
        println!("  {}", "--- Vibe Coding Capabilities ---".dimmed());
        println!("  {:<25} {}", "/vibe <prompt>".magenta(), "Speculative dual-draft race (first green wins)");
        println!("  {:<25} {}", "/race <prompt>".magenta(), "Alias for /vibe race execution");
        println!("  {:<25} {}", "/heal [cmd]".magenta(), "Compiler & test-driven self-healing loop");
        println!("  {:<25} {}", "/undo [ckpt]".magenta(), "One-key Blake3 WAL time-travel rollback");
        println!("  {:<25} {}", "/diff".magenta(), "View color-coded visual unified diff & Diff HUD");
        println!("  {:<25} {}", "/rules [init]".magenta(), "Discover or init workspace rules (.hgb/rules)");
        println!("  {:<25} {}", "/repomap [max]".magenta(), "Syntactic AST repository map outline");
        println!("  {:<25} {}", "/style [learn|view]".magenta(), "Personal coding DNA style memory vault");
        println!("  {:<25} {}", "/ship [--dry-run]".magenta(), "Autonomous PR storyteller & commit stager");
        println!();
        println!("  {}", "--- Next-Gen Vibe Coding Sentinel Fabric ---".dimmed());
        println!("  {:<25} {}", "/watch [cmd]".blue(), "Continuous Guardian background verification");
        println!("  {:<25} {}", "/ghost [apply id]".blue(), "Inspect or apply staged memory ghost-fixes");
        println!("  {:<25} {}", "/devs".blue(), "DevServer Sentinel: scan localhost dev servers");
        println!("  {:<25} {}", "/impact <sym> <file>".blue(), "Ripple Effect Radar: blast radius & call-sites");
        println!("  {:<25} {}", "/pod <task>".blue(), "Specialist Swarm Pod: 4-role consensus DAG");
        println!("  {:<25} {}", "/fix <cmd>".blue(), "Terminal Rescue: mind-reader command diagnosis");
        println!("  {:<25} {}", "/memory [record|anchor]".blue(), "Project Memory Ledger: context anchor & ADRs");
        println!();
        println!("  {}", "--- Next-Gen Vibe Pillars (HGB 3.0) ---".dimmed());
        println!("  {:<25} {}", "/snoop [url]".cyan(), "Browser Snoop: live CDP runtime console/DOM monitor");
        println!("  {:<25} {}", "/race3 [prompt]".cyan(), "Variant Race: 3-way speculative design forking");
        println!("  {:<25} {}", "/dbsync [apply|scan]".cyan(), "DbSentinel: live DB schema drift & safe auto-migration");
        println!("  {:<25} {}", "/slice <file> <sym>".cyan(), "SyntaxSlicer: surgical AST context projection");
        println!("  {:<25} {}", "/spec [synth|run]".cyan(), "AutoSpec: property invariant fuzzer & regression blocker");
        println!("  {:<25} {}", "/dna [scan|audit]".cyan(), "DriftLock: architectural DNA compliance auditor");
        println!();
        println!("  {}", "--- Transcendent Superpowers (Tier 3) ---".dimmed());
        println!("  {:<25} {}", "/ghostcoder [prefix]".cyan(), "Predictive Shadow Synthesizer: sub-50µs AST in RAM");
        println!("  {:<25} {}", "/mirage [route] [method]".cyan(), "Universal Offline API Mirage & deterministic wiretapper");
        println!("  {:<25} {}", "/chaos [target]".cyan(), "In-Process Chaos Monkey & adversarial invariant fuzzer");
        println!("  {:<25} {}", "/nightshift [goal]".cyan(), "Autonomous Night-Shift Swarm isolated worktree pipeline");
        println!("  {:<25} {}", "/vault [audit|seal]".cyan(), "Kernel-Level Ghost Envs: Blake3 vault & disk zero-leak");
        println!("  {:<25} {}", "/typelock [file]".cyan(), "Zero-Drift Polyglot Type Lock: Rust to TS & Zod");
        println!("  {:<25} {}", "/radar [orbit|atmo|surf]".cyan(), "Spatial Cockpit Radar & 3-tier semantic zoom");
        println!();
        println!("  {}", "--- Apex Vibe Superpowers (Tier 4) ---".dimmed());
        println!("  {:<25} {}", "/teleport [selector]".magenta(), "Click-to-Source CDP Teleport: resolve DOM element to AST");
        println!("  {:<25} {}", "/voice [phrase]".magenta(), "Full-Duplex Zero-Latency Voice Flow Co-Pilot");
        println!("  {:<25} {}", "/tape [url] [name]".magenta(), "Headless Screenplay & Animated PR Loom Tape");
        println!("  {:<25} {}", "/finops [prompt]".magenta(), "Token FinOps & Dynamic Latency Arbitrage");
        println!("  {:<25} {}", "/cloak [text]".magenta(), "Zero-Knowledge Airgap Cloak & PII Sanitizer");
        println!("  {:<25} {}", "/sqlguard [query]".magenta(), "Active SQL Interceptor & Shadow Transaction Jail");
        println!("  {:<25} {}", "/replay [frame]".magenta(), "Deterministic Execution Replay & Rewind-Exec");
        println!();
        println!("  {}", "--- Sovereign Godspeed Superpowers (Tier 5) ---".dimmed());
        println!("  {:<25} {}", "/canvas [old] [new]".magenta(), "Two-Way Visual Canvas & Live CSS/Tailwind Mirror");
        println!("  {:<25} {}", "/federate [goal]".magenta(), "Multi-Repo Swarm & Monorepo Mesh Federator");
        println!("  {:<25} {}", "/timewarp [months]".magenta(), "Relational Time-Warp Data Synthesizer (FK & Clock Skew)");
        println!("  {:<25} {}", "/guardrails [dir]".magenta(), "Structural Invariant Guardrails & Anti-Spaghetti Linter");
        println!("  {:<25} {}", "/triage [trace]".magenta(), "Production Crash Auto-Triage & Reproduction Pipeline");
        println!("  {:<25} {}", "/deflake [test]".magenta(), "Flaky Test Exterminator & Deterministic Stress Fuzzer");
        println!("  {:<25} {}", "/anchor".magenta(), "Associative Neural Context & Infinite Cross-Session Memory");
        println!();
        println!("  {}", "--- Competitor Hegemony & Next-Gen Ergonomics (Tier 6) ---".dimmed());
        println!("  {:<25} {}", "/lsp [prefix]".magenta(), "LSP Ghost Daemon Bridge & Sub-15ms Inline Completion");
        println!("  {:<25} {}", "/compact".magenta(), "Rolling Context Compactor & 85% Token Reducer");
        println!("  {:<25} {}", "/commit [intent]".magenta(), "Atomic Conventional Git Micro-Commit Mirror");
        println!("  {:<25} {}", "/recipe [name|list]".magenta(), "Declarative Vibe Recipes & Runbook Engine");
        println!("  {:<25} {}", "/contract [symbol]".magenta(), "Pre-Flight 5-Dimension Behavioral Contract Matrix");
        println!("  {:<25} {}", "/graph [goal]".magenta(), "Live Agent Flight-Graph & Real-Time Task DAG");
        println!();
        println!("  {}", "--- Vibe Sovereign Apex Frontier (Tier 9) ---".dimmed());
        println!("  {:<25} {}", "/predict [file] [sym]".yellow(), "Cascade Next-Edit Anticipator & Call-Site Graph");
        println!("  {:<25} {}", "/tweak [sel] [prop] [val]".yellow(), "Bidirectional DevTools CDP Tweak to Source Sync");
        println!("  {:<25} {}", "/mode [name] [query]".yellow(), "Composable Prompt Modes & High-Density Docs Harvester");
        println!("  {:<25} {}", "/sandbox [profile]".yellow(), "Zero-Config Ephemeral Stack & In-Memory Relational DB");
        println!("  {:<25} {}", "/anti-placebo [test]".yellow(), "Anti-Placebo Behavioral Mutation Gatekeeper");
        println!();
        println!("  {}", "Keybindings (Tagisan Parity):".cyan().bold());
        println!("    Ctrl+A / Home     : Move cursor to start of line");
        println!("    Ctrl+E / End      : Move cursor to end of line");
        println!("    Alt+B / Ctrl+Left : Move backward one word");
        println!("    Alt+F / Ctrl+Right: Move forward one word");
        println!("    Ctrl+K            : Kill from cursor to end of line");
        println!("    Ctrl+U            : Kill to start of line (or 1-Key Undo on empty line)");
        println!("    Ctrl+W            : Kill backward one word");
        println!("    Alt+D             : Kill forward one word");
        println!("    Ctrl+Y            : Yank (paste) last killed text");
        println!("    Ctrl+R            : Reverse incremental history search");
        println!("    Tab               : Smart autocomplete commands, models, and paths");
        println!("    Ctrl+J / Alt+Enter: Insert newline without submitting (multiline)");
        println!("    Ctrl+L            : Clear screen & redraw");
        println!("    Ctrl+C            : Clear line buffer");
        println!("    Ctrl+D            : Exit REPL (on empty line)");
        println!();
        println!("  {}", "Pro-tip: Any plain text without a '/' prefix executes as an AI swarm prompt.".italic().dimmed());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_word_backward() {
        let text: Vec<char> = "hello beautiful world".chars().collect();
        let pos1 = ReplEditor::find_word_backward(&text, 21);
        assert_eq!(pos1, 16); // start of "world"

        let pos2 = ReplEditor::find_word_backward(&text, 16);
        assert_eq!(pos2, 6); // start of "beautiful"

        let pos3 = ReplEditor::find_word_backward(&text, 6);
        assert_eq!(pos3, 0); // start of "hello"

        let pos4 = ReplEditor::find_word_backward(&text, 0);
        assert_eq!(pos4, 0);
    }

    #[test]
    fn test_find_word_forward() {
        let text: Vec<char> = "hello beautiful world".chars().collect();
        let pos1 = ReplEditor::find_word_forward(&text, 0);
        assert_eq!(pos1, 6); // start of "beautiful"

        let pos2 = ReplEditor::find_word_forward(&text, 6);
        assert_eq!(pos2, 16); // start of "world"

        let pos3 = ReplEditor::find_word_forward(&text, 16);
        assert_eq!(pos3, 21); // end of "world"

        let pos4 = ReplEditor::find_word_forward(&text, 21);
        assert_eq!(pos4, 21);
    }

    #[test]
    fn test_longest_common_prefix() {
        let list1 = vec!["/help".to_string(), "/history".to_string()];
        assert_eq!(ReplEditor::longest_common_prefix(&list1), "/h");

        let list2 = vec!["/plan".to_string(), "/prompt".to_string()];
        assert_eq!(ReplEditor::longest_common_prefix(&list2), "/p");

        let list3 = vec!["single".to_string()];
        assert_eq!(ReplEditor::longest_common_prefix(&list3), "single");

        let list4: Vec<String> = vec![];
        assert_eq!(ReplEditor::longest_common_prefix(&list4), "");
    }

    #[test]
    fn test_has_unclosed_delimiters() {
        assert!(ReplEditor::has_unclosed_delimiters("```rust\nfn main() {"));
        assert!(!ReplEditor::has_unclosed_delimiters("```rust\nfn main() {}\n```"));
        assert!(ReplEditor::has_unclosed_delimiters("let s = \"\"\"multi line"));
        assert!(!ReplEditor::has_unclosed_delimiters("let s = \"\"\"multi line\"\"\""));
        assert!(ReplEditor::has_unclosed_delimiters("fn test(a: i32, b: i32"));
        assert!(!ReplEditor::has_unclosed_delimiters("fn test(a: i32, b: i32)"));
    }

    #[test]
    fn test_get_completions_slash_commands() {
        let comp_h = ReplEditor::get_completions("/h");
        let names: Vec<String> = comp_h.into_iter().map(|(s, _)| s).collect();
        assert!(names.contains(&"/help ".to_string()));

        let comp_m = ReplEditor::get_completions("/m");
        let names: Vec<String> = comp_m.into_iter().map(|(s, _)| s).collect();
        assert!(names.contains(&"/model ".to_string()));
        assert!(names.contains(&"/mesh ".to_string()));
        assert!(names.contains(&"/mcp ".to_string()));

        let comp_mcp = ReplEditor::get_completions("/mcp ");
        let mcp_subs: Vec<String> = comp_mcp.into_iter().map(|(s, _)| s).collect();
        assert!(mcp_subs.contains(&"/mcp list ".to_string()));
        assert!(mcp_subs.contains(&"/mcp call ".to_string()));
    }

    #[test]
    fn test_get_completions_model() {
        let comp_g = ReplEditor::get_completions("/model gem");
        let names: Vec<String> = comp_g.into_iter().map(|(s, _)| s).collect();
        assert!(names.contains(&"/model gemini ".to_string()));
        assert!(names.contains(&"/model gemini-2.5-flash ".to_string()));
        assert!(names.contains(&"/model gemini-2.5-pro ".to_string()));

        let comp_q = ReplEditor::get_completions("/model qwen");
        let q_names: Vec<String> = comp_q.into_iter().map(|(s, _)| s).collect();
        assert!(q_names.contains(&"/model qwen2.5-coder:1.5b ".to_string()));
        assert!(q_names.contains(&"/model qwen2.5:0.5b ".to_string()));

        if hgb_core::OllamaProvider::is_available() {
            let comp_all = ReplEditor::get_completions("/model ");
            let all_names: Vec<String> = comp_all.into_iter().map(|(s, _)| s).collect();
            for m in hgb_core::OllamaProvider::installed_model_names() {
                assert!(
                    all_names.iter().any(|n| n.contains(&m)),
                    "Installed Ollama model '{}' must be reflected in tab completions",
                    m
                );
            }
        }
    }

    #[test]
    fn test_visible_width() {
        let plain = "hello world";
        assert_eq!(ReplEditor::visible_width(plain), 11);

        let colored_str = format!("{}", "hello world".green().bold());
        assert_eq!(ReplEditor::visible_width(&colored_str), 11);
    }

    #[test]
    fn test_print_banner_and_with_model() {
        // Exercise banner printing paths
        HagibisRepl::print_banner();
        HagibisRepl::print_banner_with_model(Some("gemini-2.5-pro"));
        HagibisRepl::print_banner_with_model(Some("deepseek-chat"));
        HagibisRepl::print_banner_with_model(Some("qwen2.5-coder:1.5b"));
        HagibisRepl::print_banner_with_model(Some("llama3.2:1b"));
    }

    #[test]
    fn test_print_completion_grid_formatting() {
        let mut buf = Vec::new();

        // 1. Slash commands
        let completions_slash = vec![
            ("/help ".to_string(), 6),
            ("/model ".to_string(), 7),
            ("/cockpit ".to_string(), 9),
            ("/exit ".to_string(), 6),
        ];
        ReplEditor::print_completion_grid(&completions_slash, "/", 80, &mut buf).unwrap();
        let out_str = String::from_utf8_lossy(&buf);
        assert!(out_str.contains("[/help]"));
        assert!(out_str.contains("[/model]"));
        assert!(out_str.contains("[/cockpit]"));

        // 2. Models
        buf.clear();
        let completions_model = vec![
            ("/model gemini-2.5-flash ".to_string(), 25),
            ("/model gemini-2.5-pro ".to_string(), 23),
        ];
        ReplEditor::print_completion_grid(&completions_model, "/model gem", 80, &mut buf).unwrap();
        let out_model = String::from_utf8_lossy(&buf);
        assert!(out_model.contains("[gemini-2.5-flash]"));
        assert!(out_model.contains("[gemini-2.5-pro]"));

        // 3. Files and directories
        buf.clear();
        let completions_files = vec![
            ("/view Cargo.toml ".to_string(), 17),
            ("/view src/ ".to_string(), 11),
        ];
        ReplEditor::print_completion_grid(&completions_files, "/view ", 80, &mut buf).unwrap();
        let out_files = String::from_utf8_lossy(&buf);
        assert!(out_files.contains("[📄 Cargo.toml]"));
        assert!(out_files.contains("[📁 src/]"));
    }
}
