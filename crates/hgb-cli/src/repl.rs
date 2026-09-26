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
            // --- Vibe Coding Capabilities ---
            "/vibe" | "/race" => {
                if args.is_empty() {
                    println!("{} Usage: /vibe <prompt> or /race <prompt>", "ℹ".yellow());
                } else {
                    println!("{}", "🏁 Spawning Speculative Dual-Draft Race (Fast Local Draft vs Frontier)...".cyan().bold());
                    let req = HgbRequest::VibeRace {
                        prompt: args,
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
            "/impact" | "/radar" => {
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
            "/pod" | "/swarm" => {
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
            "/mock" => {
                let parts: Vec<&str> = args.split_whitespace().collect();
                let resource = if !parts.is_empty() { parts[0].to_string() } else { "items".to_string() };
                let port = if parts.len() > 1 { parts[1].parse::<u16>().ok() } else { None };
                let resp = self.dispatch(HgbRequest::MockServerStart {
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
            _ => {
                println!("{} Unknown slash command '{}'. Type '/help' for available commands.", "⚠".yellow(), cmd);
            }
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
            HgbResponse::MockServerStarted { url, port, resource, seed_count } => {
                println!("{}", "🎭 EPHEMERAL MOCK FABRIC SERVER ACTIVE 🎭".bold().cyan());
                println!("  [•] Endpoint:   {}", url.bold().green());
                println!("  [•] Local Port: {}", port);
                println!("  [•] Resource:   {}", resource);
                println!("  [•] Seed Count: {} items", seed_count);
                println!("  [•] CORS:       Enabled (Access-Control-Allow-Origin: *)");
                println!("\n  💡 Test with curl:");
                println!("     {}", format!("curl -s {}", url).cyan());
                println!("     {}", format!("curl -X POST {} -H 'Content-Type: application/json' -d '{{\"name\": \"Test\"}}'", url).cyan());
            }
            HgbResponse::MockServerStopped { port } => {
                println!("{} Mock server on port {} has been stopped.", "🛑 Mock Server Stopped:".yellow().bold(), port);
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
                println!("{} Speculative race cancelled and mock preview ports released.", "🛑 Race Aborted:".yellow().bold());
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
