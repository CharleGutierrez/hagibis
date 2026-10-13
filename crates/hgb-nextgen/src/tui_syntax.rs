//! High-Performance Ratatui TUI Syntax Highlighting & Markdown Parser
//!
//! Converts raw source code strings and Markdown chat responses into beautifully
//! colorized, structured `ratatui::text::Span` elements with full keyword, type,
//! string, comment, number, operator, and macro coloring. Auto-detects un-fenced
//! code blocks so raw code in chat responses is rendered in styled code boxes.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

/// Syntactic token categories for terminal display
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeToken<'a> {
    Keyword(&'a str),
    Type(&'a str),
    StringLiteral(&'a str),
    Comment(&'a str),
    Number(&'a str),
    FunctionCall(&'a str),
    MacroCall(&'a str),
    Operator(&'a str),
    Punctuation(&'a str),
    Whitespace(&'a str),
    Plain(&'a str),
}

impl<'a> CodeToken<'a> {
    pub fn text(&self) -> &'a str {
        match self {
            Self::Keyword(s)
            | Self::Type(s)
            | Self::StringLiteral(s)
            | Self::Comment(s)
            | Self::Number(s)
            | Self::FunctionCall(s)
            | Self::MacroCall(s)
            | Self::Operator(s)
            | Self::Punctuation(s)
            | Self::Whitespace(s)
            | Self::Plain(s) => s,
        }
    }

    pub fn to_span(&self) -> Span<'static> {
        let text = self.text().to_string();
        match self {
            Self::Keyword(_) => Span::styled(
                text,
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Self::Type(_) => Span::styled(text, Style::default().fg(Color::Yellow)),
            Self::StringLiteral(_) => Span::styled(text, Style::default().fg(Color::Green)),
            Self::Comment(_) => Span::styled(
                text,
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ),
            Self::Number(_) => Span::styled(text, Style::default().fg(Color::LightCyan)),
            Self::FunctionCall(_) => Span::styled(text, Style::default().fg(Color::LightBlue)),
            Self::MacroCall(_) => Span::styled(
                text,
                Style::default()
                    .fg(Color::LightBlue)
                    .add_modifier(Modifier::BOLD),
            ),
            Self::Operator(_) => Span::styled(text, Style::default().fg(Color::Cyan)),
            Self::Punctuation(_) => Span::styled(text, Style::default().fg(Color::White)),
            Self::Whitespace(_) => Span::raw(text),
            Self::Plain(_) => Span::styled(text, Style::default().fg(Color::White)),
        }
    }
}

/// Tokenize a line of source code into typed `CodeToken`s
pub fn tokenize_code_line<'a>(line: &'a str, lang: Option<&str>) -> Vec<CodeToken<'a>> {
    let norm_lang = lang.map(detect_code_language).unwrap_or("code");
    let mut tokens = Vec::new();
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let num_chars = chars.len();
    let mut idx = 0;

    while idx < num_chars {
        let byte_pos = chars[idx].0;
        let c = chars[idx].1;

        // 1. Comments:
        // // (Rust, Zig, JS/TS, C/C++)
        // # (Python, Bash, YAML, TOML)
        // -- (SQL)
        if (c == '/' && idx + 1 < num_chars && chars[idx + 1].1 == '/')
            || (c == '#' && matches!(norm_lang, "python" | "bash" | "code"))
            || (c == '-' && idx + 1 < num_chars && chars[idx + 1].1 == '-' && matches!(norm_lang, "sql" | "code"))
        {
            tokens.push(CodeToken::Comment(&line[byte_pos..]));
            break;
        }

        // Zig multiline string literals: \\
        if norm_lang == "zig" && c == '\\' && idx + 1 < num_chars && chars[idx + 1].1 == '\\' {
            tokens.push(CodeToken::StringLiteral(&line[byte_pos..]));
            break;
        }

        // 2. Block comment /* ... */
        if c == '/' && idx + 1 < num_chars && chars[idx + 1].1 == '*' {
            let start = byte_pos;
            idx += 2;
            while idx + 1 < num_chars && !(chars[idx].1 == '*' && chars[idx + 1].1 == '/') {
                idx += 1;
            }
            if idx + 1 < num_chars {
                idx += 2;
                let end = if idx < num_chars { chars[idx].0 } else { line.len() };
                tokens.push(CodeToken::Comment(&line[start..end]));
            } else {
                tokens.push(CodeToken::Comment(&line[start..]));
                break;
            }
            continue;
        }

        // 3. String & Char Literals:
        // "...", '...', `...` (backticks for JS/TS/Bash/Go)
        // Python / Rust prefixes: b"...", r"...", f"...", u"...", etc.
        let is_quote = c == '"' || c == '\'' || c == '`';
        let is_prefixed_quote = if !is_quote && (c == 'b' || c == 'r' || c == 'f' || c == 'u' || c == 'B' || c == 'R' || c == 'F' || c == 'U') {
            if idx + 1 < num_chars && (chars[idx + 1].1 == '"' || chars[idx + 1].1 == '\'' || chars[idx + 1].1 == '`' || chars[idx + 1].1 == '#') {
                true
            } else if idx + 2 < num_chars && (chars[idx + 1].1 == 'r' || chars[idx + 1].1 == 'R') && (chars[idx + 2].1 == '"' || chars[idx + 2].1 == '\'') {
                true // fr"..." or rf"..."
            } else {
                false
            }
        } else {
            false
        };

        if is_quote || is_prefixed_quote {
            let start = byte_pos;
            let quote = if is_prefixed_quote {
                idx += 1;
                if idx < num_chars && (chars[idx].1 == 'r' || chars[idx].1 == 'R') {
                    idx += 1;
                }
                while idx < num_chars && chars[idx].1 == '#' {
                    idx += 1;
                }
                if idx < num_chars { chars[idx].1 } else { '"' }
            } else {
                c
            };
            idx += 1;
            let mut escaped = false;
            while idx < num_chars {
                let cur = chars[idx].1;
                if escaped {
                    escaped = false;
                } else if cur == '\\' {
                    escaped = true;
                } else if cur == quote {
                    idx += 1;
                    break;
                }
                idx += 1;
            }
            let end = if idx < num_chars { chars[idx].0 } else { line.len() };
            let raw_str = &line[start..end];

            // In JSON: if a string literal is followed by a colon ':', it's an object key!
            if norm_lang == "json" {
                let mut peek = idx;
                while peek < num_chars && chars[peek].1.is_whitespace() {
                    peek += 1;
                }
                if peek < num_chars && chars[peek].1 == ':' {
                    tokens.push(CodeToken::Type(raw_str));
                    continue;
                }
            }

            tokens.push(CodeToken::StringLiteral(raw_str));
            continue;
        }

        // 4. Zig builtins starting with '@' (e.g. @import, @as)
        if c == '@' && idx + 1 < num_chars && (chars[idx + 1].1.is_alphabetic() || chars[idx + 1].1 == '_') {
            let start = byte_pos;
            idx += 1;
            while idx < num_chars && (chars[idx].1.is_alphanumeric() || chars[idx].1 == '_') {
                idx += 1;
            }
            let end = if idx < num_chars { chars[idx].0 } else { line.len() };
            tokens.push(CodeToken::MacroCall(&line[start..end]));
            continue;
        }

        // 5. Bash variables: $VAR, ${VAR}, $?, $@, $*, $#, $0..$9
        if c == '$' && matches!(norm_lang, "bash" | "code") {
            let start = byte_pos;
            idx += 1;
            if idx < num_chars && chars[idx].1 == '{' {
                idx += 1;
                while idx < num_chars && chars[idx].1 != '}' {
                    idx += 1;
                }
                if idx < num_chars {
                    idx += 1;
                }
            } else if idx < num_chars && matches!(chars[idx].1, '?' | '!' | '@' | '*' | '#' | '$' | '0'..='9') {
                idx += 1;
            } else {
                while idx < num_chars && (chars[idx].1.is_alphanumeric() || chars[idx].1 == '_') {
                    idx += 1;
                }
            }
            let end = if idx < num_chars { chars[idx].0 } else { line.len() };
            if end > start + 1 {
                tokens.push(CodeToken::Type(&line[start..end]));
                continue;
            }
        }

        // 6. Numbers: digits, 0x.., 0b.., 0o..
        if c.is_ascii_digit()
            || (c == '.' && idx + 1 < num_chars && chars[idx + 1].1.is_ascii_digit() && (idx == 0 || !chars[idx - 1].1.is_alphanumeric()))
        {
            let start = byte_pos;
            if c == '0' && idx + 1 < num_chars && matches!(chars[idx + 1].1, 'x' | 'X' | 'b' | 'B' | 'o' | 'O') {
                idx += 2;
            }
            while idx < num_chars && (chars[idx].1.is_ascii_alphanumeric() || chars[idx].1 == '.' || chars[idx].1 == '_') {
                idx += 1;
            }
            let end = if idx < num_chars { chars[idx].0 } else { line.len() };
            tokens.push(CodeToken::Number(&line[start..end]));
            continue;
        }

        // 7. Identifiers and Keywords
        if c.is_alphabetic() || c == '_' {
            let start = byte_pos;
            while idx < num_chars && (chars[idx].1.is_alphanumeric() || chars[idx].1 == '_') {
                idx += 1;
            }
            let end = if idx < num_chars { chars[idx].0 } else { line.len() };
            let word = &line[start..end];

            let is_macro = idx < num_chars && chars[idx].1 == '!';
            let is_func = if is_macro {
                false
            } else {
                let mut peek = idx;
                while peek < num_chars && chars[peek].1.is_whitespace() {
                    peek += 1;
                }
                peek < num_chars && chars[peek].1 == '('
            };

            if is_macro {
                idx += 1;
                let macro_end = if idx < num_chars { chars[idx].0 } else { line.len() };
                tokens.push(CodeToken::MacroCall(&line[start..macro_end]));
            } else if is_keyword(word, norm_lang) {
                tokens.push(CodeToken::Keyword(word));
            } else if is_builtin_type(word, norm_lang) || (norm_lang != "sql" && starts_with_uppercase(word)) {
                tokens.push(CodeToken::Type(word));
            } else if is_func {
                tokens.push(CodeToken::FunctionCall(word));
            } else {
                tokens.push(CodeToken::Plain(word));
            }
            continue;
        }

        // 8. Multi-character Operators: &mut, ->, =>, ::, ==, !=, <=, >=, &&, ||, ..
        if c == '&' && idx + 3 < num_chars && chars[idx + 1].1 == 'm' && chars[idx + 2].1 == 'u' && chars[idx + 3].1 == 't' {
            let start = byte_pos;
            idx += 4;
            let end = if idx < num_chars { chars[idx].0 } else { line.len() };
            tokens.push(CodeToken::Operator(&line[start..end]));
            continue;
        }

        if idx + 1 < num_chars {
            let c2 = chars[idx + 1].1;
            let two = match (c, c2) {
                ('-', '>') | ('=', '>') | (':', ':') | ('=', '=') | ('!', '=')
                | ('<', '=') | ('>', '=') | ('&', '&') | ('|', '|') | ('.', '.')
                | ('+', '=') | ('-', '=') | ('*', '=') | ('/', '=') | ('<', '<') | ('>', '>') => true,
                _ => false,
            };
            if two {
                let start = byte_pos;
                idx += 2;
                let end = if idx < num_chars { chars[idx].0 } else { line.len() };
                tokens.push(CodeToken::Operator(&line[start..end]));
                continue;
            }
        }

        // Single operators
        if matches!(c, '+' | '-' | '*' | '/' | '%' | '=' | '<' | '>' | '&' | '|' | '^' | '!' | '?' | '~') {
            let start = byte_pos;
            idx += 1;
            let end = if idx < num_chars { chars[idx].0 } else { line.len() };
            tokens.push(CodeToken::Operator(&line[start..end]));
            continue;
        }

        // Delimiters & punctuation
        if matches!(c, '{' | '}' | '(' | ')' | '[' | ']' | ',' | ';' | ':' | '.') {
            let start = byte_pos;
            idx += 1;
            let end = if idx < num_chars { chars[idx].0 } else { line.len() };
            tokens.push(CodeToken::Punctuation(&line[start..end]));
            continue;
        }

        // Whitespace
        if c.is_whitespace() {
            let start = byte_pos;
            while idx < num_chars && chars[idx].1.is_whitespace() {
                idx += 1;
            }
            let end = if idx < num_chars { chars[idx].0 } else { line.len() };
            tokens.push(CodeToken::Whitespace(&line[start..end]));
            continue;
        }

        // Fallback plain char
        let start = byte_pos;
        idx += 1;
        let end = if idx < num_chars { chars[idx].0 } else { line.len() };
        tokens.push(CodeToken::Plain(&line[start..end]));
    }

    tokens
}

/// Highlight a single line into a vector of Spans
pub fn highlight_code_line(line: &str, lang: Option<&str>) -> Vec<Span<'static>> {
    let tokens = tokenize_code_line(line, lang);
    if tokens.is_empty() {
        vec![Span::raw(line.to_string())]
    } else {
        tokens.into_iter().map(|t| t.to_span()).collect()
    }
}

/// Highlight a code line and wrap it across multiple lines respecting max_width and preserving indentation
pub fn highlight_and_wrap_code_line(
    line: &str,
    max_width: usize,
    lang: Option<&str>,
) -> Vec<Vec<Span<'static>>> {
    if max_width == 0 {
        return vec![highlight_code_line(line, lang)];
    }

    let tokens = tokenize_code_line(line, lang);
    wrap_tokens_to_width(&tokens, max_width)
}

/// Wraps token stream into rows where visual width <= max_width, carrying indent for continuation lines
pub fn wrap_tokens_to_width(tokens: &[CodeToken], max_width: usize) -> Vec<Vec<Span<'static>>> {
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut current_row: Vec<Span<'static>> = Vec::new();
    let mut current_width: usize = 0;

    // Detect initial indent
    let mut initial_indent = String::new();
    if let Some(CodeToken::Whitespace(ws)) = tokens.first() {
        initial_indent = ws.to_string();
    }
    let continuation_indent = format!("{}  ", initial_indent);

    for token in tokens {
        let text = token.text();
        let tok_w = UnicodeWidthStr::width(text);

        if current_width + tok_w <= max_width || current_row.is_empty() {
            current_row.push(token.to_span());
            current_width += tok_w;
        } else {
            // Push finished row
            rows.push(std::mem::take(&mut current_row));
            // Start next row with continuation indent
            let indent_w = UnicodeWidthStr::width(continuation_indent.as_str());
            current_row.push(Span::raw(continuation_indent.clone()));
            current_row.push(token.to_span());
            current_width = indent_w + tok_w;
        }
    }

    if !current_row.is_empty() {
        rows.push(current_row);
    }

    if rows.is_empty() {
        rows.push(vec![Span::raw("")]);
    }

    rows
}

/// Markdown item representation for Cockpit rendering
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownItem<'a> {
    Line(&'a str),
    CodeBlock {
        lang: &'a str,
        lines: Vec<&'a str>,
    },
}

/// Parse raw chat response into MarkdownItems, auto-detecting both fenced blocks and raw code blocks
pub fn parse_markdown<'a>(content: &'a str) -> Vec<MarkdownItem<'a>> {
    let mut items = Vec::new();
    let mut in_fence = false;
    let mut fence_lang = "";
    let mut fence_lines = Vec::new();

    let all_lines: Vec<&'a str> = content.lines().collect();
    let mut i = 0;

    while i < all_lines.len() {
        let line = all_lines[i];

        // 1. Explicit Markdown Code Fence ```
        if line.trim_start().starts_with("```") {
            if in_fence {
                in_fence = false;
                items.push(MarkdownItem::CodeBlock {
                    lang: fence_lang,
                    lines: std::mem::take(&mut fence_lines),
                });
            } else {
                in_fence = true;
                fence_lang = line.trim_start().trim_start_matches("```").trim();
                fence_lines.clear();
            }
            i += 1;
            continue;
        }

        if in_fence {
            fence_lines.push(line);
            i += 1;
            continue;
        }

        // 2. Auto-detect unfenced code block (sequence of code lines)
        if is_code_line(line) {
            let mut code_lines = vec![line];
            let mut j = i + 1;
            while j < all_lines.len() {
                let next_line = all_lines[j];
                if next_line.trim_start().starts_with("```") {
                    break;
                }
                if is_code_line(next_line) || (next_line.trim().is_empty() && j + 1 < all_lines.len() && is_code_line(all_lines[j + 1])) {
                    code_lines.push(next_line);
                    j += 1;
                } else {
                    break;
                }
            }

            // Only convert to code block if at least 2 consecutive code lines
            if code_lines.len() >= 2 {
                let detected = detect_code_language_lines(&code_lines);
                items.push(MarkdownItem::CodeBlock {
                    lang: detected,
                    lines: code_lines,
                });
                i = j;
                continue;
            }
        }

        items.push(MarkdownItem::Line(line));
        i += 1;
    }

    if in_fence {
        items.push(MarkdownItem::CodeBlock {
            lang: fence_lang,
            lines: fence_lines,
        });
    }

    items
}

/// Heuristic score to determine if a line of text is programming code
pub fn score_code_line(line: &str) -> i32 {
    let t = line.trim();
    if t.is_empty() {
        return 0;
    }
    let mut score = 0;
    if line.starts_with("    ") || line.starts_with('\t') {
        score += 1;
    }
    if t.starts_with("//") || t.starts_with("/*") || t.starts_with("#") || t.starts_with("--") {
        score += 2;
    }
    if t.ends_with('{') || t.ends_with(';') || t.ends_with('}') || t.starts_with('}') || t.starts_with('{') {
        score += 2;
    }
    if t.contains("=>") || t.contains("->") || t.contains("::") || t.contains("&mut") {
        score += 3;
    }
    if t.contains('(') && t.contains(')') {
        score += 2;
    }
    if t.contains('.') && !t.contains(' ') {
        score += 2;
    }
    if t.starts_with("fn ") || t.starts_with("pub fn ") || t.starts_with("let ") || t.starts_with("mut ")
        || t.starts_with("match ") || t.starts_with("for ") || t.starts_with("if ") || t.starts_with("struct ")
        || t.starts_with("enum ") || t.starts_with("impl ") || t.starts_with("use ") || t.starts_with("import ")
        || t.starts_with("def ") || t.starts_with("class ") || t.starts_with("return ")
    {
        score += 3;
    }
    score
}

/// Returns true if a line looks like source code
pub fn is_code_line(line: &str) -> bool {
    score_code_line(line) >= 2
}

/// Inspects lines of code to identify the programming language
pub fn detect_code_language_lines(lines: &[&str]) -> &'static str {
    let mut rust_score = 0;
    let mut python_score = 0;
    let mut js_ts_score = 0;
    let mut zig_score = 0;
    let mut bash_score = 0;
    let mut json_score = 0;
    let mut sql_score = 0;

    for line in lines {
        let t = line.trim();
        // Rust
        if t.starts_with("fn ") || t.contains("->") || t.contains("::") || t.contains("&mut") || t.contains("match ") || t.contains("let mut ") {
            rust_score += 3;
        }
        // Python
        if t.starts_with("def ") || t.starts_with("import ") || t.starts_with("from ") || (t.ends_with(':') && (t.starts_with("if ") || t.starts_with("for ") || t.starts_with("while "))) {
            python_score += 3;
        }
        // JS/TS
        if t.starts_with("const ") || t.starts_with("function ") || t.contains("=>") || t.starts_with("export ") || t.contains("console.log") {
            js_ts_score += 2;
        }
        // Zig
        if t.contains("pub fn") || t.contains("!void") || t.contains("@import") || t.starts_with("const std =") {
            zig_score += 4;
        }
        // Bash
        if t.starts_with("#!/") || t.starts_with("echo ") || t.contains("grep ") || t.contains("| awk") || t.starts_with("export ") || t.starts_with("set -e") {
            bash_score += 3;
        }
        // JSON
        if (t.starts_with('{') || t.starts_with('[')) || (t.starts_with('"') && t.contains("\":")) {
            json_score += 3;
        }
        // SQL
        let upper = t.to_ascii_uppercase();
        if upper.starts_with("SELECT ") || upper.starts_with("INSERT INTO ") || upper.starts_with("CREATE TABLE ") || upper.starts_with("UPDATE ") || upper.starts_with("DELETE FROM ") {
            sql_score += 4;
        }
    }

    let mut scores = [
        ("rust", rust_score),
        ("zig", zig_score),
        ("python", python_score),
        ("typescript", js_ts_score),
        ("bash", bash_score),
        ("json", json_score),
        ("sql", sql_score),
    ];
    scores.sort_by(|a, b| b.1.cmp(&a.1));
    if scores[0].1 > 0 {
        scores[0].0
    } else {
        "code"
    }
}

/// Detect code language from file extension or hint
pub fn detect_code_language(hint: &str) -> &'static str {
    match hint.to_lowercase().as_str() {
        "rs" | "rust" => "rust",
        "py" | "python" => "python",
        "js" | "javascript" => "javascript",
        "ts" | "typescript" => "typescript",
        "zig" => "zig",
        "go" => "go",
        "c" => "c",
        "cpp" | "c++" => "cpp",
        "sh" | "bash" | "zsh" => "bash",
        "json" => "json",
        "sql" => "sql",
        "html" => "html",
        "css" => "css",
        _ => "code",
    }
}

fn is_keyword(word: &str, lang: &str) -> bool {
    let lower_buf;
    let w = if lang == "sql" {
        lower_buf = word.to_ascii_lowercase();
        lower_buf.as_str()
    } else {
        word
    };

    match lang {
        "rust" => matches!(
            w,
            "as" | "async" | "await" | "break" | "const" | "continue" | "crate" | "dyn"
                | "else" | "enum" | "extern" | "false" | "fn" | "for" | "if" | "impl" | "in"
                | "let" | "loop" | "match" | "mod" | "move" | "mut" | "pub" | "ref"
                | "return" | "self" | "Self" | "static" | "struct" | "super" | "trait"
                | "true" | "type" | "unsafe" | "use" | "where" | "while"
        ),
        "zig" => matches!(
            w,
            "addrspace" | "align" | "allowzero" | "and" | "anyframe" | "anytype" | "asm"
                | "async" | "await" | "break" | "catch" | "comptime" | "const" | "continue"
                | "defer" | "else" | "enum" | "errdefer" | "error" | "export" | "extern"
                | "fn" | "for" | "if" | "inline" | "noalias" | "noinline" | "nosuspend"
                | "opaque" | "or" | "orelse" | "packed" | "pub" | "resume" | "return"
                | "struct" | "suspend" | "switch" | "test" | "threadlocal" | "try" | "union"
                | "unreachable" | "usingnamespace" | "var" | "volatile" | "while"
        ),
        "python" => matches!(
            w,
            "and" | "as" | "assert" | "async" | "await" | "break" | "class" | "continue"
                | "def" | "del" | "elif" | "else" | "except" | "finally" | "for" | "from"
                | "global" | "if" | "import" | "in" | "is" | "lambda" | "nonlocal" | "not"
                | "or" | "pass" | "raise" | "return" | "try" | "while" | "with" | "yield"
        ),
        "javascript" | "typescript" => matches!(
            w,
            "abstract" | "any" | "as" | "async" | "await" | "break" | "case" | "catch"
                | "class" | "const" | "continue" | "debugger" | "default" | "delete" | "do"
                | "else" | "enum" | "export" | "extends" | "finally" | "for" | "from"
                | "function" | "if" | "implements" | "import" | "in" | "instanceof"
                | "interface" | "let" | "new" | "of" | "package" | "private" | "protected"
                | "public" | "readonly" | "return" | "static" | "super" | "switch" | "this"
                | "throw" | "try" | "type" | "typeof" | "var" | "void" | "while" | "with"
                | "yield"
        ),
        "bash" => matches!(
            w,
            "alias" | "case" | "declare" | "do" | "done" | "echo" | "elif" | "else" | "esac"
                | "eval" | "exec" | "exit" | "export" | "fi" | "for" | "function" | "if"
                | "in" | "local" | "readonly" | "return" | "select" | "set" | "shift"
                | "source" | "test" | "then" | "time" | "trap" | "typeset" | "unalias"
                | "unset" | "until" | "while"
        ),
        "sql" => matches!(
            w,
            "all" | "alter" | "and" | "as" | "asc" | "begin" | "between" | "by" | "cascade"
                | "case" | "check" | "commit" | "constraint" | "count" | "create" | "cross"
                | "database" | "default" | "delete" | "desc" | "distinct" | "drop" | "else"
                | "end" | "exists" | "foreign" | "from" | "full" | "group" | "having"
                | "ilike" | "in" | "index" | "inner" | "insert" | "into" | "is" | "join"
                | "key" | "left" | "like" | "limit" | "not" | "null" | "offset" | "on" | "or"
                | "order" | "outer" | "primary" | "references" | "right" | "rollback"
                | "schema" | "select" | "set" | "table" | "then" | "transaction" | "truncate"
                | "union" | "unique" | "update" | "values" | "view" | "when" | "where" | "with"
        ),
        _ => matches!(
            w,
            "fn" | "let" | "mut" | "match" | "if" | "else" | "return" | "struct" | "enum"
                | "impl" | "pub" | "use" | "unsafe" | "async" | "await" | "for" | "in"
                | "while" | "loop" | "break" | "continue" | "const" | "static" | "type"
                | "dyn" | "trait" | "mod" | "as" | "ref" | "move" | "where" | "crate"
                | "super" | "self" | "def" | "class" | "import" | "from" | "export"
                | "function" | "val" | "var" | "package" | "func" | "interface" | "select"
                | "insert" | "update" | "delete" | "throw" | "try" | "catch" | "finally"
                | "yield" | "lambda" | "with" | "pass" | "assert" | "case" | "switch"
                | "default" | "typedef" | "inline" | "volatile" | "echo"
        ),
    }
}

fn is_builtin_type(word: &str, lang: &str) -> bool {
    let lower_buf;
    let w = if lang == "sql" {
        lower_buf = word.to_ascii_lowercase();
        lower_buf.as_str()
    } else {
        word
    };

    match lang {
        "rust" => matches!(
            w,
            "u8" | "u16" | "u32" | "u64" | "u128" | "usize"
                | "i8" | "i16" | "i32" | "i64" | "i128" | "isize"
                | "f32" | "f64" | "bool" | "char" | "str"
                | "String" | "Vec" | "Option" | "Result" | "Some" | "None" | "Ok" | "Err"
                | "Self" | "Box" | "Rc" | "Arc" | "RefCell" | "Mutex" | "RwLock"
                | "HashMap" | "HashSet" | "BTreeMap" | "BTreeSet"
        ),
        "zig" => matches!(
            w,
            "bool" | "void" | "noreturn" | "type" | "anyerror" | "anyopaque"
                | "u8" | "u16" | "u32" | "u64" | "u128" | "usize"
                | "i8" | "i16" | "i32" | "i64" | "i128" | "isize"
                | "f16" | "f32" | "f64" | "f80" | "f128"
                | "c_short" | "c_ushort" | "c_int" | "c_uint" | "c_long" | "c_ulong"
                | "c_longlong" | "c_ulonglong" | "c_longdouble" | "c_void"
        ),
        "python" => matches!(
            w,
            "True" | "False" | "None" | "int" | "float" | "str" | "bool" | "list"
                | "dict" | "set" | "tuple" | "bytes" | "self" | "cls" | "Exception"
                | "range" | "object"
        ),
        "javascript" | "typescript" => matches!(
            w,
            "true" | "false" | "null" | "undefined" | "NaN" | "Infinity"
                | "string" | "number" | "boolean" | "symbol" | "bigint" | "any" | "unknown"
                | "never" | "void" | "Object" | "Array" | "Promise" | "Map" | "Set" | "Record"
        ),
        "sql" => matches!(
            w,
            "int" | "integer" | "bigint" | "smallint" | "tinyint" | "varchar" | "char"
                | "text" | "boolean" | "bool" | "date" | "time" | "timestamp" | "datetime"
                | "numeric" | "decimal" | "real" | "float" | "double" | "serial"
                | "bigserial" | "json" | "jsonb" | "blob" | "bytea" | "uuid"
        ),
        "json" => matches!(w, "true" | "false" | "null"),
        _ => matches!(
            w,
            "u8" | "u16" | "u32" | "u64" | "u128" | "usize"
                | "i8" | "i16" | "i32" | "i64" | "i128" | "isize"
                | "f32" | "f64" | "bool" | "char" | "str"
                | "String" | "Vec" | "Option" | "Result" | "Some" | "None" | "Ok" | "Err"
                | "Self" | "Box" | "Rc" | "Arc" | "RefCell" | "Mutex" | "RwLock"
                | "HashMap" | "HashSet" | "BTreeMap" | "BTreeSet"
                | "True" | "False"
        ),
    }
}

fn starts_with_uppercase(word: &str) -> bool {
    word.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
        && word.chars().skip(1).any(|c| c.is_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_highlight_rust_line() {
        let line = "let mut buffer = [0u8; 1024]; // Init buffer";
        let spans = highlight_code_line(line, Some("rust"));
        assert!(!spans.is_empty());
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, line);
    }

    #[test]
    fn test_parse_markdown_with_code_block() {
        let md = "Here is some code:\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```\nDone.";
        let items = parse_markdown(md);
        assert_eq!(items.len(), 3);
        match &items[1] {
            MarkdownItem::CodeBlock { lang, lines } => {
                assert_eq!(*lang, "rust");
                assert_eq!(lines.len(), 3);
            }
            _ => panic!("Expected CodeBlock"),
        }
    }

    #[test]
    fn test_auto_detect_unfenced_code() {
        let raw = "fn infect(computer: &mut dyn dyn) -> u64 {\n    let mut s = Vec::new();\n    s.len()\n}";
        let items = parse_markdown(raw);
        assert_eq!(items.len(), 1);
        match &items[0] {
            MarkdownItem::CodeBlock { lang, lines } => {
                assert_eq!(*lang, "rust");
                assert_eq!(lines.len(), 4);
            }
            _ => panic!("Expected auto-detected CodeBlock"),
        }
    }

    #[test]
    fn test_wrap_code_line() {
        let line = "    let extremely_long_variable_name_with_lots_of_details = [0u8; 1024];";
        let wrapped = highlight_and_wrap_code_line(line, 35, Some("rust"));
        assert!(wrapped.len() >= 2);
    }

    #[test]
    fn test_zig_tokenization() {
        let line = "pub fn main() !void { const std = @import(\"std\"); }";
        let tokens = tokenize_code_line(line, Some("zig"));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Keyword("pub"))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Keyword("fn"))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Type("void"))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Keyword("const"))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::MacroCall("@import"))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::StringLiteral("\"std\""))));

        // Multiline zig string
        let ml = "\\\\Hello from multiline Zig string";
        let ml_tokens = tokenize_code_line(ml, Some("zig"));
        assert!(matches!(ml_tokens[0], CodeToken::StringLiteral(_)));
    }

    #[test]
    fn test_python_tokenization() {
        let line = "def calculate_sum(items: list) -> int: # calculate sum";
        let tokens = tokenize_code_line(line, Some("python"));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Keyword("def"))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Type("list"))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Type("int"))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Comment("# calculate sum"))));

        let f_str = "msg = f\"Hello, {name}!\"";
        let f_tokens = tokenize_code_line(f_str, Some("python"));
        assert!(f_tokens.iter().any(|t| matches!(t, CodeToken::StringLiteral("f\"Hello, {name}!\""))));
    }

    #[test]
    fn test_javascript_typescript_tokenization() {
        let line = "export interface User { id: number; name: string; }";
        let tokens = tokenize_code_line(line, Some("typescript"));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Keyword("export"))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Keyword("interface"))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Type("number"))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Type("string"))));

        // Template literal with backticks
        let tmpl = "const greeting = `Hello ${name}`;";
        let tmpl_tokens = tokenize_code_line(tmpl, Some("javascript"));
        assert!(tmpl_tokens.iter().any(|t| matches!(t, CodeToken::Keyword("const"))));
        assert!(tmpl_tokens.iter().any(|t| matches!(t, CodeToken::StringLiteral("`Hello ${name}`"))));
    }

    #[test]
    fn test_bash_tokenization() {
        let line = "export PORT=${CUSTOM_PORT:-8080} # Set default port";
        let tokens = tokenize_code_line(line, Some("bash"));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Keyword("export"))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Type("${CUSTOM_PORT:-8080}"))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Comment("# Set default port"))));
    }

    #[test]
    fn test_json_tokenization() {
        let line = "{\"name\": \"Alice\", \"count\": 42, \"active\": true, \"data\": null}";
        let tokens = tokenize_code_line(line, Some("json"));
        // Object keys in JSON are highlighted as Type tokens
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Type("\"name\""))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::StringLiteral("\"Alice\""))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Type("\"count\""))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Number("42"))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Type("true"))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Type("null"))));
    }

    #[test]
    fn test_sql_tokenization() {
        let line = "SELECT id, username FROM users WHERE active = true; -- get users";
        let tokens = tokenize_code_line(line, Some("sql"));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Keyword("SELECT"))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Keyword("FROM"))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Keyword("WHERE"))));
        assert!(tokens.iter().any(|t| matches!(t, CodeToken::Comment("-- get users"))));

        // Lowercase SQL
        let lower_line = "select * from accounts where balance > 100.50";
        let lower_tokens = tokenize_code_line(lower_line, Some("sql"));
        assert!(lower_tokens.iter().any(|t| matches!(t, CodeToken::Keyword("select"))));
        assert!(lower_tokens.iter().any(|t| matches!(t, CodeToken::Keyword("from"))));
        assert!(lower_tokens.iter().any(|t| matches!(t, CodeToken::Keyword("where"))));
        assert!(lower_tokens.iter().any(|t| matches!(t, CodeToken::Number("100.50"))));
    }

    #[test]
    fn test_malformed_code_safety_no_panics() {
        // Unclosed string
        let unclosed = "let x = \"unclosed string without closing quote";
        let spans = highlight_code_line(unclosed, Some("rust"));
        assert!(!spans.is_empty());

        // Unclosed block comment
        let unclosed_comment = "/* unclosed comment at EOF";
        let spans2 = highlight_code_line(unclosed_comment, Some("rust"));
        assert!(!spans2.is_empty());

        // UTF-8 multibyte, emoji, and weird operators
        let weird = "fn 🦀_function() -> 🚀 { let αβγ = 42.0; \"🌟 hello\"; }";
        let spans3 = highlight_code_line(weird, Some("rust"));
        assert!(!spans3.is_empty());
        let reconstructed: String = spans3.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(reconstructed, weird);

        // Extreme wrap width 1
        let wrapped_tight = highlight_and_wrap_code_line("fn foo() {}", 1, Some("rust"));
        assert!(!wrapped_tight.is_empty());
    }
}
