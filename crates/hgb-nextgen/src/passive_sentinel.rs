use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};

/// Type of event emitted by the Passive Sentinel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SentinelEventKind {
    SymbolAdded,
    SymbolModified,
    SymbolRemoved,
    SyntaxMalformation,
    FileHealthy,
}

/// A specific change event detected by the Passive Sentinel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentinelEvent {
    pub file_path: PathBuf,
    pub kind: SentinelEventKind,
    pub symbol_name: Option<String>,
    pub details: String,
    pub has_syntax_error: bool,
    pub timestamp_ms: u64,
}

/// Current status of the Passive Sentinel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SentinelHealthStatus {
    Watching,
    Verifying,
    Warning(String),
    Healthy,
}

impl SentinelHealthStatus {
    pub fn badge(&self) -> &'static str {
        match self {
            Self::Watching => "👁️ WATCHING",
            Self::Verifying => "⚡ VERIFYING",
            Self::Warning(_) => "⚠️ SYNTAX ALERT",
            Self::Healthy => "🟢 IN SYNC",
        }
    }
}

/// Complete telemetry state of the Passive Sentinel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SentinelTelemetry {
    pub status: SentinelHealthStatus,
    pub total_inspections: usize,
    pub last_event: Option<SentinelEvent>,
    pub tracked_files_count: usize,
    pub active_syntax_errors: Vec<String>,
}

/// Symbol snapshot representation.
#[derive(Debug, Clone)]
struct SymbolSnapshot {
    signature: String,
    hash: u64,
}

/// Internal file state tracked by the sentinel.
#[derive(Debug, Clone)]
struct FileSnapshot {
    symbols: HashMap<String, SymbolSnapshot>,
    _last_modified: Instant,
    content_hash: u64,
}

/// The Continuous Background Sentinel.
#[derive(Debug)]
pub struct PassiveSentinel {
    pub workspace_root: PathBuf,
    pub debounce_duration: Duration,
    file_snapshots: HashMap<PathBuf, FileSnapshot>,
    total_inspections: usize,
    current_status: SentinelHealthStatus,
    last_event: Option<SentinelEvent>,
    active_syntax_errors: Vec<String>,
    last_change_time: Option<Instant>,
}

impl PassiveSentinel {
    /// Creates a new PassiveSentinel with custom debounce duration.
    pub fn new(root: PathBuf, debounce_ms: u64) -> Self {
        Self {
            workspace_root: root,
            debounce_duration: Duration::from_millis(debounce_ms),
            file_snapshots: HashMap::new(),
            total_inspections: 0,
            current_status: SentinelHealthStatus::Watching,
            last_event: None,
            active_syntax_errors: Vec::new(),
            last_change_time: None,
        }
    }

    /// Default 150ms debounce sentinel.
    pub fn default_sentinel(root: PathBuf) -> Self {
        Self::new(root, 150)
    }

    /// Initializes snapshot of a file without triggering events.
    pub fn register_file(&mut self, path: &Path, content: &str) {
        let symbols = extract_symbols(content);
        let hash = simple_hash(content);
        self.file_snapshots.insert(
            path.to_path_buf(),
            FileSnapshot {
                symbols,
                _last_modified: Instant::now(),
                content_hash: hash,
            },
        );
    }

    /// Evaluates a detected file change against existing snapshot.
    pub fn evaluate_change(&mut self, path: &Path, new_content: &str) -> Option<SentinelEvent> {
        self.total_inspections += 1;
        self.current_status = SentinelHealthStatus::Verifying;
        self.last_change_time = Some(Instant::now());

        // 1. Quick Syntax Balance Check (Braces, quotes, parentheses)
        if let Some(err) = check_syntax_integrity(new_content) {
            let event = SentinelEvent {
                file_path: path.to_path_buf(),
                kind: SentinelEventKind::SyntaxMalformation,
                symbol_name: None,
                details: err.clone(),
                has_syntax_error: true,
                timestamp_ms: current_timestamp_ms(),
            };
            self.current_status = SentinelHealthStatus::Warning(err.clone());
            self.active_syntax_errors.push(format!("{}: {}", path.display(), err));
            self.last_event = Some(event.clone());
            return Some(event);
        }

        // 2. Compute symbol delta
        let new_symbols = extract_symbols(new_content);
        let new_hash = simple_hash(new_content);

        let event = if let Some(old_snap) = self.file_snapshots.get(path) {
            if old_snap.content_hash == new_hash {
                self.current_status = SentinelHealthStatus::Healthy;
                return None;
            }

            // Check for added symbols
            let mut detected_event = None;
            for (name, new_sym) in &new_symbols {
                match old_snap.symbols.get(name) {
                    None => {
                        detected_event = Some(SentinelEvent {
                            file_path: path.to_path_buf(),
                            kind: SentinelEventKind::SymbolAdded,
                            symbol_name: Some(name.clone()),
                            details: format!("Added symbol '{}' with signature '{}'", name, new_sym.signature),
                            has_syntax_error: false,
                            timestamp_ms: current_timestamp_ms(),
                        });
                        break;
                    }
                    Some(old_sym) if old_sym.hash != new_sym.hash => {
                        detected_event = Some(SentinelEvent {
                            file_path: path.to_path_buf(),
                            kind: SentinelEventKind::SymbolModified,
                            symbol_name: Some(name.clone()),
                            details: format!("Modified symbol '{}'", name),
                            has_syntax_error: false,
                            timestamp_ms: current_timestamp_ms(),
                        });
                        break;
                    }
                    _ => {}
                }
            }

            // Check for removed symbols
            if detected_event.is_none() {
                for (name, _) in &old_snap.symbols {
                    if !new_symbols.contains_key(name) {
                        detected_event = Some(SentinelEvent {
                            file_path: path.to_path_buf(),
                            kind: SentinelEventKind::SymbolRemoved,
                            symbol_name: Some(name.clone()),
                            details: format!("Removed symbol '{}'", name),
                            has_syntax_error: false,
                            timestamp_ms: current_timestamp_ms(),
                        });
                        break;
                    }
                }
            }

            detected_event.unwrap_or_else(|| SentinelEvent {
                file_path: path.to_path_buf(),
                kind: SentinelEventKind::FileHealthy,
                symbol_name: None,
                details: "File updated cleanly".to_string(),
                has_syntax_error: false,
                timestamp_ms: current_timestamp_ms(),
            })
        } else {
            SentinelEvent {
                file_path: path.to_path_buf(),
                kind: SentinelEventKind::FileHealthy,
                symbol_name: None,
                details: "Initial tracking registered".to_string(),
                has_syntax_error: false,
                timestamp_ms: current_timestamp_ms(),
            }
        };

        // Update stored snapshot
        self.file_snapshots.insert(
            path.to_path_buf(),
            FileSnapshot {
                symbols: new_symbols,
                _last_modified: Instant::now(),
                content_hash: new_hash,
            },
        );

        // Clear active error for this file if healthy
        self.active_syntax_errors.retain(|e| !e.starts_with(&path.display().to_string()));
        self.current_status = SentinelHealthStatus::Healthy;
        self.last_event = Some(event.clone());
        Some(event)
    }

    /// Obtains current status and telemetry.
    pub fn telemetry(&self) -> SentinelTelemetry {
        SentinelTelemetry {
            status: self.current_status.clone(),
            total_inspections: self.total_inspections,
            last_event: self.last_event.clone(),
            tracked_files_count: self.file_snapshots.len(),
            active_syntax_errors: self.active_syntax_errors.clone(),
        }
    }
}

/// Simple brace and delimiter integrity checker.
fn check_syntax_integrity(code: &str) -> Option<String> {
    let mut brace_stack = Vec::new();
    let mut in_string = false;
    let mut in_char = false;
    let mut prev_char = ' ';

    for (idx, ch) in code.char_indices() {
        if in_string {
            if ch == '"' && prev_char != '\\' {
                in_string = false;
            }
        } else if in_char {
            if ch == '\'' && prev_char != '\\' {
                in_char = false;
            }
        } else {
            match ch {
                '"' => in_string = true,
                '\'' => in_char = true,
                '{' | '(' | '[' => brace_stack.push((ch, idx)),
                '}' => {
                    if let Some((top, _)) = brace_stack.pop() {
                        if top != '{' {
                            return Some(format!("Mismatched delimiter: expected '}}', found '{}' after opening '{}'", ch, top));
                        }
                    } else {
                        return Some("Unmatched closing '}' without opening '{'".to_string());
                    }
                }
                ')' => {
                    if let Some((top, _)) = brace_stack.pop() {
                        if top != '(' {
                            return Some(format!("Mismatched delimiter: expected ')', found '{}' after opening '{}'", ch, top));
                        }
                    } else {
                        return Some("Unmatched closing ')' without opening '('".to_string());
                    }
                }
                ']' => {
                    if let Some((top, _)) = brace_stack.pop() {
                        if top != '[' {
                            return Some(format!("Mismatched delimiter: expected ']', found '{}' after opening '{}'", ch, top));
                        }
                    } else {
                        return Some("Unmatched closing ']' without opening '['".to_string());
                    }
                }
                _ => {}
            }
        }
        prev_char = ch;
    }

    if in_string {
        return Some("Unterminated string literal".to_string());
    }
    if !brace_stack.is_empty() {
        let (unclosed, _) = brace_stack.last().unwrap();
        return Some(format!("Unclosed delimiter '{}' at EOF", unclosed));
    }

    None
}

/// Fast regex-free symbol extractor for Rust/TS/Python.
fn extract_symbols(code: &str) -> HashMap<String, SymbolSnapshot> {
    let mut map = HashMap::new();
    let lines: Vec<&str> = code.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let trimmed = lines[i].trim();
        // fn or pub fn
        if (trimmed.starts_with("fn ") || trimmed.starts_with("pub fn ") || trimmed.starts_with("pub async fn ") || trimmed.starts_with("async fn "))
            && trimmed.contains('(')
        {
            let sig_end = trimmed.find('{').unwrap_or(trimmed.len());
            let sig = trimmed[..sig_end].trim();
            if let Some(open) = sig.find('(') {
                let name_part = sig[..open].split_whitespace().last().unwrap_or("");
                if !name_part.is_empty() {
                    let mut body = String::new();
                    let mut depth = 0;
                    let mut found_open = false;
                    for line in &lines[i..] {
                        body.push_str(line);
                        body.push('\n');
                        for ch in line.chars() {
                            if ch == '{' {
                                depth += 1;
                                found_open = true;
                            } else if ch == '}' {
                                depth -= 1;
                            }
                        }
                        if found_open && depth <= 0 {
                            break;
                        }
                    }
                    map.insert(
                        name_part.to_string(),
                        SymbolSnapshot {
                            signature: sig.to_string(),
                            hash: simple_hash(&body),
                        },
                    );
                }
            }
        } else if trimmed.starts_with("pub struct ") || trimmed.starts_with("struct ")
            || trimmed.starts_with("pub enum ") || trimmed.starts_with("enum ")
        {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() >= 2 {
                let name = parts[if parts[0] == "pub" { 2 } else { 1 }].trim_matches('{');
                let mut body = String::new();
                let mut depth = 0;
                let mut found_open = false;
                for line in &lines[i..] {
                    body.push_str(line);
                    body.push('\n');
                    for ch in line.chars() {
                        if ch == '{' {
                            depth += 1;
                            found_open = true;
                        } else if ch == '}' {
                            depth -= 1;
                        }
                    }
                    if found_open && depth <= 0 {
                        break;
                    }
                }
                map.insert(
                    name.to_string(),
                    SymbolSnapshot {
                        signature: trimmed.to_string(),
                        hash: simple_hash(&body),
                    },
                );
            }
        }
        i += 1;
    }
    map
}

fn simple_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for byte in s.bytes() {
        h ^= byte as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
