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
pub fn tokenize_code_line<'a>(line: &'a str, _lang: Option<&str>) -> Vec<CodeToken<'a>> {
    let mut tokens = Vec::new();
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let num_chars = chars.len();
    let mut idx = 0;

    while idx < num_chars {
        let byte_pos = chars[idx].0;
        let c = chars[idx].1;

        // 1. Comments: //, #, --
        if (c == '/' && idx + 1 < num_chars && chars[idx + 1].1 == '/')
            || c == '#'
            || (c == '-' && idx + 1 < num_chars && chars[idx + 1].1 == '-')
        {
            tokens.push(CodeToken::Comment(&line[byte_pos..]));
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

        // 3. String & Char Literals: "...", '...', b"...", r"..."
        if c == '"'
            || c == '\''
            || (c == 'b' && idx + 1 < num_chars && (chars[idx + 1].1 == '"' || chars[idx + 1].1 == '\''))
            || (c == 'r' && idx + 1 < num_chars && (chars[idx + 1].1 == '"' || chars[idx + 1].1 == '#'))
        {
            let start = byte_pos;
            let quote = if c == 'b' || c == 'r' {
                idx += 1;
                if idx < num_chars && chars[idx].1 == '#' {
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
            tokens.push(CodeToken::StringLiteral(&line[start..end]));
            continue;
        }

        // 4. Numbers: digits, 0x.., 0b..
        if c.is_ascii_digit()
            || (c == '.' && idx + 1 < num_chars && chars[idx + 1].1.is_ascii_digit() && (idx == 0 || !chars[idx - 1].1.is_alphanumeric()))
        {
            let start = byte_pos;
            if c == '0' && idx + 1 < num_chars && matches!(chars[idx + 1].1, 'x' | 'X' | 'b' | 'o') {
                idx += 2;
            }
            while idx < num_chars && (chars[idx].1.is_ascii_alphanumeric() || chars[idx].1 == '.' || chars[idx].1 == '_') {
                idx += 1;
            }
            let end = if idx < num_chars { chars[idx].0 } else { line.len() };
            tokens.push(CodeToken::Number(&line[start..end]));
            continue;
        }

        // 5. Identifiers and Keywords
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
            } else if is_func && !is_keyword(word) && !is_builtin_type(word) {
                tokens.push(CodeToken::FunctionCall(word));
            } else if is_keyword(word) {
                tokens.push(CodeToken::Keyword(word));
            } else if is_builtin_type(word) || starts_with_uppercase(word) {
                tokens.push(CodeToken::Type(word));
            } else {
                tokens.push(CodeToken::Plain(word));
            }
            continue;
        }

        // 6. Multi-character Operators: &mut, ->, =>, ::, ==, !=, <=, >=, &&, ||, ..
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

    for line in lines {
        let t = line.trim();
        if t.starts_with("fn ") || t.contains("->") || t.contains("::") || t.contains("&mut") || t.contains("match ") {
            rust_score += 3;
        }
        if t.starts_with("def ") || t.starts_with("import ") || t.starts_with("from ") || t.ends_with(':') {
            python_score += 3;
        }
        if t.starts_with("const ") || t.starts_with("function ") || t.contains("=>") || t.starts_with("import ") {
            js_ts_score += 2;
        }
        if t.contains("pub fn") || t.contains("!void") || t.contains("@import") {
            zig_score += 3;
        }
    }

    if rust_score >= python_score && rust_score >= js_ts_score && rust_score >= zig_score && rust_score > 0 {
        "rust"
    } else if python_score > js_ts_score && python_score > 0 {
        "python"
    } else if js_ts_score > 0 {
        "typescript"
    } else if zig_score > 0 {
        "zig"
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

fn is_keyword(word: &str) -> bool {
    matches!(
        word,
        "fn" | "let" | "mut" | "match" | "if" | "else" | "return" | "struct"
            | "enum" | "impl" | "pub" | "use" | "unsafe" | "async" | "await"
            | "for" | "in" | "while" | "loop" | "break" | "continue" | "const"
            | "static" | "type" | "dyn" | "trait" | "mod" | "as" | "ref"
            | "move" | "where" | "crate" | "super" | "self" | "def" | "class"
            | "import" | "from" | "export" | "function" | "val" | "var"
            | "package" | "func" | "interface" | "select" | "insert" | "update"
            | "delete" | "throw" | "try" | "catch" | "finally" | "yield"
            | "lambda" | "with" | "pass" | "assert" | "case" | "switch"
            | "default" | "typedef" | "inline" | "volatile"
    )
}

fn is_builtin_type(word: &str) -> bool {
    matches!(
        word,
        "u8" | "u16" | "u32" | "u64" | "u128" | "usize"
            | "i8" | "i16" | "i32" | "i64" | "i128" | "isize"
            | "f32" | "f64" | "bool" | "char" | "str"
            | "String" | "Vec" | "Option" | "Result" | "Some" | "None" | "Ok" | "Err"
            | "Self" | "Box" | "Rc" | "Arc" | "RefCell" | "Mutex" | "RwLock"
            | "HashMap" | "HashSet" | "BTreeMap" | "BTreeSet"
            | "true" | "false" | "null" | "nil" | "undefined"
    )
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
}
