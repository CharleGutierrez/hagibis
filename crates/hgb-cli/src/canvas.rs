//! # AGY Chat Canvas & Inline Tool Call Cards for Hagibis (`hgb`)
//!
//! Provides authentic Google Antigravity (AGY) terminal formatting:
//! 1. `ToolCallCard`: Stylized Unicode boxed cards for tool and command executions.
//! 2. `ChatCanvas`: Rich terminal Markdown renderer with fenced code boxes,
//!    syntax-colored diffs (`+` green, `-` red), thinking streams, tables, and blockquotes.

use colored::Colorize;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Status of an inline tool call execution
#[derive(Debug, Clone, PartialEq)]
pub enum ToolCardStatus {
    Running,
    Success { duration_ms: u64, exit_code: i32 },
    Failed { duration_ms: u64, error: String },
}

impl ToolCardStatus {
    pub fn badge(&self) -> String {
        match self {
            Self::Running => "[RUNNING]".yellow().bold().to_string(),
            Self::Success { exit_code, .. } => {
                if *exit_code == 0 {
                    "[DONE]".green().bold().to_string()
                } else {
                    format!("[EXIT {}]", exit_code).yellow().bold().to_string()
                }
            }
            Self::Failed { .. } => "[FAILED]".red().bold().to_string(),
        }
    }

    pub fn duration_str(&self) -> Option<String> {
        match self {
            Self::Running => None,
            Self::Success { duration_ms, .. } => Some(format!("{}ms", duration_ms)),
            Self::Failed { duration_ms, .. } => Some(format!("{}ms", duration_ms)),
        }
    }
}

/// An inline tool execution card displayed in the REPL and Chat Canvas
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallCard {
    pub tool_name: String,
    pub summary: String,
    pub status: ToolCardStatus,
    pub output_snippet: Option<String>,
    pub extra_details: Option<String>,
}

impl ToolCallCard {
    pub fn new(
        tool_name: impl Into<String>,
        summary: impl Into<String>,
        status: ToolCardStatus,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            summary: summary.into(),
            status,
            output_snippet: None,
            extra_details: None,
        }
    }

    pub fn with_output(mut self, output: impl Into<String>) -> Self {
        self.output_snippet = Some(output.into());
        self
    }

    pub fn with_details(mut self, details: impl Into<String>) -> Self {
        self.extra_details = Some(details.into());
        self
    }

    /// Render tool call card into a beautiful AGY Unicode boxed frame
    pub fn render_box(&self, max_width: usize) -> String {
        let tool_icon = match self.tool_name.as_str() {
            "run_command" | "exec" | "sh" | "bash" => "💻",
            "view_file" | "cat" | "read" => "📖",
            "write_to_file" | "write" => "📝",
            "replace_file_content" | "edit" => "✂️ ",
            "find_by_name" | "find" => "🔍",
            "grep_search" | "grep" => "🔎",
            "list_dir" | "ls" => "📁",
            "invoke_subagent" | "subagent" => "🤖",
            _ => "🔧",
        };

        // Header raw width
        let header_prefix_raw = format!("╭─── {} {} ", tool_icon, self.tool_name);
        let prefix_len = UnicodeWidthStr::width(header_prefix_raw.as_str());

        // Status raw width
        let badge_raw = match &self.status {
            ToolCardStatus::Running => "[RUNNING]",
            ToolCardStatus::Success { exit_code, .. } => {
                if *exit_code == 0 {
                    "[DONE]"
                } else {
                    "[EXIT code]"
                }
            }
            ToolCardStatus::Failed { .. } => "[FAILED]",
        };
        let dur_raw = match self.status.duration_str() {
            Some(d) => format!("  ⏱️ {}", d),
            None => "".to_string(),
        };
        let target_raw = if !self.summary.is_empty() {
            format!("  🎯 {}", self.summary)
        } else {
            "".to_string()
        };
        let status_raw_len = 11
            + UnicodeWidthStr::width(badge_raw)
            + UnicodeWidthStr::width(dur_raw.as_str())
            + UnicodeWidthStr::width(target_raw.as_str());

        let mut natural_w = (prefix_len + 6).max(status_raw_len + 4);

        if let ToolCardStatus::Failed { ref error, .. } = self.status {
            let err_w = UnicodeWidthStr::width(error.as_str()) + 14;
            if err_w > natural_w {
                natural_w = err_w;
            }
        }

        if let Some(ref details) = self.extra_details {
            let det_w = UnicodeWidthStr::width(details.as_str()) + 8;
            if det_w > natural_w {
                natural_w = det_w;
            }
        }

        if let Some(ref snippet) = self.output_snippet {
            for line in snippet.trim().lines().take(25) {
                let lw = UnicodeWidthStr::width(line) + 6;
                if lw > natural_w {
                    natural_w = lw;
                }
            }
        }

        let max_bound = if max_width < 40 { 80 } else { max_width };
        let width = natural_w.max(50).min(max_bound);
        let mut out = String::new();

        // 1. Header line: ╭─── 🔧 tool_name ───────────────────────╮
        let header_prefix = format!("╭─── {} {} ", tool_icon, self.tool_name.bold().cyan());
        let remaining = if width > prefix_len + 2 {
            width - prefix_len - 2
        } else {
            1
        };
        let dashes = "─".repeat(remaining);
        out.push_str(&format!("{}{}{}\n", header_prefix, dashes.dimmed(), "╮".dimmed()));

        // 2. Status & Details Line: │  Status: [DONE]  Duration: 14ms  Target: file.rs  │
        let badge = self.status.badge();
        let dur_part = match self.status.duration_str() {
            Some(d) => format!("  ⏱️ {}", d.cyan()),
            None => "".to_string(),
        };
        let target_part = if !self.summary.is_empty() {
            format!("  🎯 {}", self.summary.white().bold())
        } else {
            "".to_string()
        };

        let status_content_len = 11
            + UnicodeWidthStr::width(badge_raw)
            + UnicodeWidthStr::width(dur_raw.as_str())
            + UnicodeWidthStr::width(target_raw.as_str());
        let status_pad = width.saturating_sub(status_content_len + 2);
        out.push_str(&format!(
            "│  Status: {}{}{}{}{}\n",
            badge,
            dur_part,
            target_part,
            " ".repeat(status_pad),
            "│".dimmed()
        ));

        // Render error if failed
        if let ToolCardStatus::Failed { ref error, .. } = self.status {
            let err_len = UnicodeWidthStr::width(error.as_str()) + 13;
            let pad = width.saturating_sub(err_len + 2);
            out.push_str(&format!(
                "│  ✖ Error: {}{}{}\n",
                error.red().bold(),
                " ".repeat(pad),
                "│".dimmed()
            ));
        }

        // Extra details if present
        if let Some(ref details) = self.extra_details {
            let det_len = UnicodeWidthStr::width(details.as_str()) + 7;
            let pad = width.saturating_sub(det_len + 2);
            out.push_str(&format!(
                "│  ℹ️  {}{}{}\n",
                details.dimmed(),
                " ".repeat(pad),
                "│".dimmed()
            ));
        }

        // 3. Output snippet if present
        if let Some(ref snippet) = self.output_snippet {
            let trimmed = snippet.trim();
            if !trimmed.is_empty() {
                out.push_str(&format!("├{}┤\n", "─".repeat(width.saturating_sub(2)).dimmed()));
                let max_inner = width.saturating_sub(6).max(10);
                for line in trimmed.lines().take(25) {
                    let formatted_line = if line.starts_with('+') && !line.starts_with("+++") {
                        line.green().to_string()
                    } else if line.starts_with('-') && !line.starts_with("---") {
                        line.red().to_string()
                    } else if line.starts_with("error") || line.contains("Error") {
                        line.red().bold().to_string()
                    } else if line.starts_with("warning") || line.contains("Warning") {
                        line.yellow().to_string()
                    } else {
                        line.dimmed().to_string()
                    };
                    let lw = UnicodeWidthStr::width(line);
                    let (safe_line, safe_lw) = if lw > max_inner {
                        let mut tr = String::new();
                        let mut cw = 0;
                        for ch in line.chars() {
                            let w = UnicodeWidthChar::width(ch).unwrap_or(1);
                            if cw + w > max_inner.saturating_sub(1) {
                                break;
                            }
                            tr.push(ch);
                            cw += w;
                        }
                        tr.push('…');
                        (tr, cw + 1)
                    } else {
                        (formatted_line, lw)
                    };
                    let pad = max_inner.saturating_sub(safe_lw);
                    out.push_str(&format!("│  {}  {}{}\n", safe_line, " ".repeat(pad), "│".dimmed()));
                }
                let line_count = trimmed.lines().count();
                if line_count > 25 {
                    let trunc_msg = format!("... ({} more lines truncated)", line_count - 25);
                    let tw = UnicodeWidthStr::width(trunc_msg.as_str());
                    let pad = max_inner.saturating_sub(tw);
                    out.push_str(&format!("│  {}  {}{}\n", trunc_msg.dimmed(), " ".repeat(pad), "│".dimmed()));
                }
            }
        }

        // 4. Footer line: ╰──────────────────────────────────────────╯
        let footer = format!("╰{}╯", "─".repeat(width.saturating_sub(2)));
        out.push_str(&footer.dimmed().to_string());
        out.push('\n');

        out
    }

    pub fn print(&self) {
        let width = match crossterm::terminal::size() {
            Ok((w, _)) => w as usize,
            Err(_) => 80,
        };
        print!("{}", self.render_box(width));
    }
}

/// The AGY Chat Canvas terminal markdown and stream renderer
#[derive(Debug, Default, Clone)]
pub struct ChatCanvas;

impl ChatCanvas {
    pub fn new() -> Self {
        Self
    }

    /// Render Markdown text with rich terminal enhancements
    pub fn render_markdown(text: &str) -> String {
        let mut out = String::new();
        let mut in_code_block = false;
        let mut code_lang = String::new();
        let mut code_buffer: Vec<String> = Vec::new();
        let mut in_thinking = false;
        let mut thinking_buffer: Vec<String> = Vec::new();
        let mut in_table = false;
        let mut table_rows: Vec<Vec<String>> = Vec::new();

        let terminal_width = match crossterm::terminal::size() {
            Ok((w, _)) => (w as usize).clamp(60, 110),
            Err(_) => 80,
        };

        for line in text.lines() {
            // Check for thinking blocks (<thinking>...</thinking>)
            if line.contains("<thinking>") {
                in_thinking = true;
                let remainder = line.replace("<thinking>", "");
                if !remainder.trim().is_empty() {
                    thinking_buffer.push(remainder);
                }
                continue;
            }
            if line.contains("</thinking>") {
                in_thinking = false;
                let before = line.replace("</thinking>", "");
                if !before.trim().is_empty() {
                    thinking_buffer.push(before);
                }
                out.push_str(&Self::render_thinking_box(&thinking_buffer, terminal_width));
                thinking_buffer.clear();
                continue;
            }
            if in_thinking {
                thinking_buffer.push(line.to_string());
                continue;
            }

            // Check for fenced code blocks
            if line.trim_start().starts_with("```") {
                if in_code_block {
                    // Closing code block
                    in_code_block = false;
                    out.push_str(&Self::render_code_box(&code_lang, &code_buffer, terminal_width));
                    code_buffer.clear();
                    code_lang.clear();
                } else {
                    // Opening code block
                    in_code_block = true;
                    code_lang = line.trim_start().trim_start_matches('`').trim().to_string();
                }
                continue;
            }

            if in_code_block {
                code_buffer.push(line.to_string());
                continue;
            }

            // Check for Markdown table rows
            if line.trim().starts_with('|') && line.trim().ends_with('|') {
                in_table = true;
                // Check if separator line (|---|---|)
                let is_sep = line.chars().all(|c| c == '|' || c == '-' || c == ':' || c == ' ');
                if !is_sep {
                    let cols: Vec<String> = line
                        .trim()
                        .trim_matches('|')
                        .split('|')
                        .map(|s| s.trim().to_string())
                        .collect();
                    table_rows.push(cols);
                }
                continue;
            } else if in_table {
                // Table ended
                in_table = false;
                out.push_str(&Self::render_table(&table_rows));
                table_rows.clear();
            }

            // Headers
            if let Some(h1) = line.strip_prefix("# ") {
                out.push_str(&format!("\n{}\n", h1.bold().cyan().underline()));
                continue;
            }
            if let Some(h2) = line.strip_prefix("## ") {
                out.push_str(&format!("\n{}\n", h2.bold().yellow()));
                continue;
            }
            if let Some(h3) = line.strip_prefix("### ") {
                out.push_str(&format!("\n{}\n", h3.bold().green()));
                continue;
            }

            // Blockquotes
            if let Some(bq) = line.strip_prefix("> ") {
                out.push_str(&format!("  {} {}\n", "│".dimmed(), bq.italic().dimmed()));
                continue;
            }

            // Bullet points
            if line.trim_start().starts_with("* ") || line.trim_start().starts_with("- ") {
                let indent = line.len() - line.trim_start().len();
                let content = &line.trim_start()[2..];
                out.push_str(&format!("{}{} {}\n", " ".repeat(indent), "•".cyan(), content));
                continue;
            }

            // Numbered lists (1. , 2. )
            if let Some(dot_idx) = line.find(". ") {
                if dot_idx > 0 && line[..dot_idx].chars().all(|c| c.is_ascii_digit()) {
                    let num = &line[..dot_idx];
                    let content = &line[dot_idx + 2..];
                    out.push_str(&format!("  {} {}\n", format!("{}.", num).yellow().bold(), content));
                    continue;
                }
            }

            // Regular line
            out.push_str(line);
            out.push('\n');
        }

        // Flush remaining open blocks
        if in_code_block {
            out.push_str(&Self::render_code_box(&code_lang, &code_buffer, terminal_width));
        }
        if in_table && !table_rows.is_empty() {
            out.push_str(&Self::render_table(&table_rows));
        }
        if !thinking_buffer.is_empty() {
            out.push_str(&Self::render_thinking_box(&thinking_buffer, terminal_width));
        }

        out
    }

    /// Render fenced code block box
    pub fn render_code_box(lang: &str, lines: &[String], width: usize) -> String {
        let mut out = String::new();
        let lang_tag = if lang.is_empty() { "code" } else { lang };
        let is_diff = lang_tag == "diff" || lang_tag == "patch";

        let header = format!("╭─── [{}] ", lang_tag.cyan().bold());
        let header_raw = format!("╭─── [{}] ", lang_tag);
        let prefix_len = UnicodeWidthStr::width(header_raw.as_str());
        let remaining = if width > prefix_len + 2 {
            width - prefix_len - 2
        } else {
            4
        };
        let dashes = "─".repeat(remaining);
        out.push_str(&format!("{}{}{}\n", header, dashes.dimmed(), "╮".dimmed()));

        for (idx, line) in lines.iter().enumerate() {
            let line_num = format!("{:>3} │ ", idx + 1).dimmed();
            let colored_content = if is_diff {
                if line.starts_with('+') && !line.starts_with("+++") {
                    line.green().to_string()
                } else if line.starts_with('-') && !line.starts_with("---") {
                    line.red().to_string()
                } else if line.starts_with("@@") {
                    line.cyan().bold().to_string()
                } else {
                    line.dimmed().to_string()
                }
            } else {
                line.to_string()
            };
            out.push_str(&format!("│ {}{}\n", line_num, colored_content));
        }

        let footer = format!("╰{}╯", "─".repeat(width.saturating_sub(2)));
        out.push_str(&footer.dimmed().to_string());
        out.push('\n');
        out
    }

    /// Render thinking stream box
    pub fn render_thinking_box(lines: &[String], width: usize) -> String {
        let mut out = String::new();
        let total_chars: usize = lines.iter().map(|l| l.len()).sum();
        let est_tokens = total_chars / 4;

        let title = format!("💭 Reasoning Stream ({} tokens) ", est_tokens);
        let header = format!("╭─── {} ", title.magenta().bold());
        let header_raw = format!("╭─── {} ", title);
        let prefix_len = UnicodeWidthStr::width(header_raw.as_str());
        let remaining = if width > prefix_len + 2 {
            width - prefix_len - 2
        } else {
            4
        };
        let dashes = "─".repeat(remaining);
        out.push_str(&format!("{}{}{}\n", header, dashes.dimmed(), "╮".dimmed()));

        for line in lines {
            out.push_str(&format!("│  {}\n", line.italic().dimmed()));
        }

        let footer = format!("╰{}╯", "─".repeat(width.saturating_sub(2)));
        out.push_str(&footer.dimmed().to_string());
        out.push('\n');
        out
    }

    /// Render Unicode table
    pub fn render_table(rows: &[Vec<String>]) -> String {
        if rows.is_empty() {
            return String::new();
        }

        let num_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if num_cols == 0 {
            return String::new();
        }

        let mut col_widths = vec![0usize; num_cols];
        for row in rows {
            for (col_idx, cell) in row.iter().enumerate() {
                let cell_w = UnicodeWidthStr::width(cell.as_str());
                if cell_w > col_widths[col_idx] {
                    col_widths[col_idx] = cell_w;
                }
            }
        }

        // Add 2 padding chars to each column
        for w in &mut col_widths {
            *w = (*w + 2).max(4);
        }

        let mut out = String::new();

        // 1. Top border: ┌──────┬──────┐
        out.push_str("┌");
        for (i, w) in col_widths.iter().enumerate() {
            out.push_str(&"─".repeat(*w));
            if i + 1 < num_cols {
                out.push_str("┬");
            }
        }
        out.push_str("┐\n");

        // 2. Rows
        for (row_idx, row) in rows.iter().enumerate() {
            out.push_str("│");
            for (col_idx, &w) in col_widths.iter().enumerate() {
                let cell_val = row.get(col_idx).map(|s| s.as_str()).unwrap_or("");
                let cell_w = UnicodeWidthStr::width(cell_val);
                let pad = w.saturating_sub(cell_w + 1);
                if row_idx == 0 {
                    out.push_str(&format!(" {} ", cell_val.bold().cyan()));
                } else {
                    out.push_str(&format!(" {} ", cell_val));
                }
                out.push_str(&" ".repeat(pad.saturating_sub(1)));
                out.push_str("│");
            }
            out.push('\n');

            // Separator after header
            if row_idx == 0 && rows.len() > 1 {
                out.push_str("├");
                for (i, w) in col_widths.iter().enumerate() {
                    out.push_str(&"─".repeat(*w));
                    if i + 1 < num_cols {
                        out.push_str("┼");
                    }
                }
                out.push_str("┤\n");
            }
        }

        // 3. Bottom border: └──────┴──────┘
        out.push_str("└");
        for (i, w) in col_widths.iter().enumerate() {
            out.push_str(&"─".repeat(*w));
            if i + 1 < num_cols {
                out.push_str("┴");
            }
        }
        out.push_str("┘\n");

        out
    }

    /// Render Markdown directly to stdout
    pub fn print_markdown(text: &str) {
        let rendered = Self::render_markdown(text);
        print!("{}", rendered);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_call_card_rendering() {
        let card = ToolCallCard::new(
            "run_command",
            "cargo test --workspace",
            ToolCardStatus::Success {
                duration_ms: 180,
                exit_code: 0,
            },
        )
        .with_output("running 39 tests\ntest result: ok. 39 passed");

        let rendered = card.render_box(80);
        assert!(rendered.contains("run_command"));
        assert!(rendered.contains("[DONE]"));
        assert!(rendered.contains("180ms"));
        assert!(rendered.contains("cargo test --workspace"));
        assert!(rendered.contains("running 39 tests"));
    }

    #[test]
    fn test_tool_call_card_failed_state() {
        let card = ToolCallCard::new(
            "replace_file_content",
            "file=src/main.rs",
            ToolCardStatus::Failed {
                duration_ms: 12,
                error: "TargetContent not found".into(),
            },
        )
        .with_details("Attempted search on line 40-50");

        let rendered = card.render_box(80);
        assert!(rendered.contains("[FAILED]"));
        assert!(rendered.contains("12ms"));
        assert!(rendered.contains("TargetContent not found"));
    }

    #[test]
    fn test_chat_canvas_markdown_rendering() {
        let md = r#"# Main Header
## Sub Header
> Important notification note

- First item
- Second item

```rust
fn main() {
    println!("Hello Hagibis");
}
```

<thinking>
Evaluating invariant boundaries across subagents...
</thinking>
"#;

        let rendered = ChatCanvas::render_markdown(md);
        assert!(rendered.contains("Main Header"));
        assert!(rendered.contains("Sub Header"));
        assert!(rendered.contains("Important notification note"));
        assert!(rendered.contains("First item"));
        assert!(rendered.contains("rust"));
        assert!(rendered.contains("Hello Hagibis"));
        assert!(rendered.contains("Reasoning Stream"));
    }

    #[test]
    fn test_chat_canvas_diff_block() {
        let diff_md = r#"```diff
--- old.rs
+++ new.rs
@@ -1,3 +1,3 @@
-let x = 10;
+let x = 20;
```"#;

        let rendered = ChatCanvas::render_markdown(diff_md);
        assert!(rendered.contains("diff"));
        assert!(rendered.contains("-let x = 10;"));
        assert!(rendered.contains("+let x = 20;"));
    }

    #[test]
    fn test_chat_canvas_table_rendering() {
        let table_md = r#"
| Pillar | Status | Latency |
| Microkernel | Nominal | 120µs |
| Gemini | Connected | 85ms |
"#;

        let rendered = ChatCanvas::render_markdown(table_md);
        assert!(rendered.contains("Pillar"));
        assert!(rendered.contains("Microkernel"));
        assert!(rendered.contains("Nominal"));
        assert!(rendered.contains("Gemini"));
        assert!(rendered.contains("┌"));
        assert!(rendered.contains("┘"));
    }
}
