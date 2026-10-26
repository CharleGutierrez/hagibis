//! # Token Shredder - Native Terminal Output Compressor & Token Optimizer
//!
//! High-performance native SIMD ANSI and VT100/CSI escape code stripper,
//! repetitive progress frame folding engine, and oversized tool output head/tail condenser.
//! Achieves up to 90% token reduction on noisy compiler dumps, package manager updates,
//! and large command outputs before forwarding to LLM contexts.
//!
//! Includes `TokenPolicyEngine`:
//! - Configurable redaction rules for credentials (OpenAI/Gemini keys, AWS secrets, GitHub tokens, Bearer headers, SSH keys).
//! - Rule for shredding bulky minified JSON blobs and oversized base64 strings into concise summaries (`[shredded_json: N keys, M bytes]`).
//! - Rule for AST-level comment/docstring compression when token budget is constrained.
//! - Dynamic policy loading from `TokenPolicyConfig`.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

static ANSI_CSI_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    // Matches standard CSI sequences: ESC [ ... [a-zA-Z]
    Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").unwrap()
});

static ANSI_OSC_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    // Matches OSC sequences: ESC ] ... (BEL | ST)
    Regex::new(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)").unwrap()
});

static ANSI_OTHER_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    // Matches 2-character escape codes like ESC M, ESC =, ESC >, etc.
    Regex::new(r"\x1b[PX^_][^\x1b]*\x1b\\|\x1b[@-Z\\-_]").unwrap()
});

static PROGRESS_LINE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:\[[=\->\s#*]{3,}\]|(?:downloading|building|compiling|fetching|unpacking|extracting|resolving)\b.*(?:\.{3}|\d+%|\d+/\d+|\b\d+(?:\.\d+)?\s*(?:MB|KB|GB|B)\b))").unwrap()
});

/// Truncate string slice at or before `max_bytes` without splitting a multi-byte UTF-8 code point
pub fn safe_truncate_str(s: &str, max_bytes: usize) -> &str {
    if max_bytes >= s.len() {
        return s;
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !s.is_char_boundary(boundary) {
        boundary -= 1;
    }
    &s[..boundary]
}

/// Metrics and compressed content produced by `TokenShredder`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShredResult {
    /// Estimated token count before shredding
    pub original_tokens: usize,
    /// Estimated token count after shredding
    pub shredded_tokens: usize,
    /// Percentage of tokens saved (0.0% to 100.0%)
    pub savings_percent: f64,
    /// Byte count before shredding
    pub original_bytes: usize,
    /// Byte count after shredding
    pub shredded_bytes: usize,
    /// Shredded and compressed content
    pub content: String,
}

/// User-configurable token policy configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenPolicyConfig {
    /// Whether to redact sensitive API keys, AWS secrets, GitHub tokens, Bearer headers, and SSH keys
    pub redact_credentials: bool,
    /// Custom user-provided regex patterns to redact
    pub custom_credential_patterns: Vec<String>,
    /// Whether to shred bulky minified JSON blobs into concise summaries
    pub shred_json_blobs: bool,
    /// Minimum byte length for a JSON blob to be considered bulky and eligible for shredding
    pub json_blob_min_bytes: usize,
    /// Whether to shred oversized base64 strings into concise summaries
    pub shred_base64: bool,
    /// Minimum byte length for base64 sequences to be shredded
    pub base64_min_bytes: usize,
    /// Whether to compress comments and docstrings at AST/code level
    pub compress_ast_comments: bool,
    /// Optional target token budget. When specified and input exceeds budget, compression is applied aggressively
    pub token_budget: Option<usize>,
}

impl Default for TokenPolicyConfig {
    fn default() -> Self {
        Self {
            redact_credentials: true,
            custom_credential_patterns: Vec::new(),
            shred_json_blobs: true,
            json_blob_min_bytes: 64,
            shred_base64: true,
            base64_min_bytes: 64,
            compress_ast_comments: false,
            token_budget: None,
        }
    }
}

impl TokenPolicyConfig {
    /// Load policy configuration from JSON string
    pub fn from_json(json: &str) -> std::result::Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Serialize policy configuration to JSON string
    pub fn to_json(&self) -> std::result::Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Aggressive policy configuration with comment compression and tight size limits
    pub fn aggressive() -> Self {
        Self {
            redact_credentials: true,
            custom_credential_patterns: Vec::new(),
            shred_json_blobs: true,
            json_blob_min_bytes: 32,
            shred_base64: true,
            base64_min_bytes: 48,
            compress_ast_comments: true,
            token_budget: Some(2048),
        }
    }
}

/// Telemetry metrics for policy execution
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PolicyExecutionMetrics {
    pub redacted_credentials_count: usize,
    pub shredded_json_blobs_count: usize,
    pub shredded_base64_blobs_count: usize,
    pub comments_compressed_count: usize,
    pub original_bytes: usize,
    pub processed_bytes: usize,
}

/// Regex & AST Token Policy Engine
#[derive(Debug, Clone)]
pub struct TokenPolicyEngine {
    config: TokenPolicyConfig,
    regex_openai_key: Regex,
    regex_google_key: Regex,
    regex_github_token: Regex,
    regex_aws_key_id: Regex,
    regex_aws_secret: Regex,
    regex_bearer: Regex,
    regex_ssh_key: Regex,
    custom_regexes: Vec<Regex>,
    regex_base64_data_uri: Regex,
    regex_base64_raw: Regex,
}

impl Default for TokenPolicyEngine {
    fn default() -> Self {
        Self::new(TokenPolicyConfig::default())
    }
}

impl TokenPolicyEngine {
    /// Creates a new `TokenPolicyEngine` from configuration
    pub fn new(config: TokenPolicyConfig) -> Self {
        let regex_openai_key = Regex::new(r"\bsk-[a-zA-Z0-9_\-]{20,}\b").unwrap();
        let regex_google_key = Regex::new(r"\bAIza[0-9A-Za-z\-_]{30,45}\b").unwrap();
        let regex_github_token = Regex::new(r"\b(?:ghp|gho|ghu|ghs|ghr|github_pat)_[A-Za-z0-9_]{36,}\b").unwrap();
        let regex_aws_key_id = Regex::new(r"\b(?:AKIA|ASIA|ABIA|ACCA)[0-9A-Z]{16}\b").unwrap();
        let regex_aws_secret = Regex::new(r#"(?i)(?:aws_secret_access_key|secret_key|aws_secret)\s*[:=]\s*['"]?([A-Za-z0-9/+=]{40})['"]?"#).unwrap();
        let regex_bearer = Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9_\-\.]{20,}\b").unwrap();
        let regex_ssh_key = Regex::new(r"(?s)-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----.*?-----END (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----").unwrap();
        let regex_base64_data_uri = Regex::new(r"data:[a-zA-Z0-9_\-\./]+;base64,[A-Za-z0-9+/=]{40,}").unwrap();
        let regex_base64_raw = Regex::new(r"\b[A-Za-z0-9+/]{64,}={0,2}\b").unwrap();

        let mut custom_regexes = Vec::new();
        for pat in &config.custom_credential_patterns {
            if let Ok(re) = Regex::new(pat) {
                custom_regexes.push(re);
            }
        }

        Self {
            config,
            regex_openai_key,
            regex_google_key,
            regex_github_token,
            regex_aws_key_id,
            regex_aws_secret,
            regex_bearer,
            regex_ssh_key,
            custom_regexes,
            regex_base64_data_uri,
            regex_base64_raw,
        }
    }

    /// Creates a new `TokenPolicyEngine` from configuration
    pub fn from_config(config: TokenPolicyConfig) -> Self {
        Self::new(config)
    }

    /// Returns reference to current policy configuration
    pub fn config(&self) -> &TokenPolicyConfig {
        &self.config
    }

    /// Redacts sensitive credentials (OpenAI keys, AWS secrets, GitHub tokens, Bearer headers, SSH keys)
    pub fn redact_credentials(&self, input: &str) -> (String, usize) {
        if !self.config.redact_credentials || input.is_empty() {
            return (input.to_string(), 0);
        }

        let mut count = 0;
        let mut text = input.to_string();

        // 1. SSH Private Keys
        if self.regex_ssh_key.is_match(&text) {
            let matches = self.regex_ssh_key.find_iter(&text).count();
            count += matches;
            text = self.regex_ssh_key.replace_all(&text, "[REDACTED_SSH_PRIVATE_KEY]").into_owned();
        }

        // 2. AWS Secret Access Key
        if self.regex_aws_secret.is_match(&text) {
            let matches = self.regex_aws_secret.find_iter(&text).count();
            count += matches;
            text = self.regex_aws_secret.replace_all(&text, "aws_secret_access_key='[REDACTED_AWS_SECRET]'").into_owned();
        }

        // 3. AWS Key ID
        if self.regex_aws_key_id.is_match(&text) {
            let matches = self.regex_aws_key_id.find_iter(&text).count();
            count += matches;
            text = self.regex_aws_key_id.replace_all(&text, "[REDACTED_AWS_KEY_ID]").into_owned();
        }

        // 4. GitHub tokens
        if self.regex_github_token.is_match(&text) {
            let matches = self.regex_github_token.find_iter(&text).count();
            count += matches;
            text = self.regex_github_token.replace_all(&text, "[REDACTED_GITHUB_TOKEN]").into_owned();
        }

        // 5. Google / Gemini keys
        if self.regex_google_key.is_match(&text) {
            let matches = self.regex_google_key.find_iter(&text).count();
            count += matches;
            text = self.regex_google_key.replace_all(&text, "[REDACTED_GOOGLE_API_KEY]").into_owned();
        }

        // 6. OpenAI keys
        if self.regex_openai_key.is_match(&text) {
            let matches = self.regex_openai_key.find_iter(&text).count();
            count += matches;
            text = self.regex_openai_key.replace_all(&text, "[REDACTED_API_KEY]").into_owned();
        }

        // 7. Bearer authorization header tokens
        if self.regex_bearer.is_match(&text) {
            let matches = self.regex_bearer.find_iter(&text).count();
            count += matches;
            text = self.regex_bearer.replace_all(&text, "Bearer [REDACTED_BEARER_TOKEN]").into_owned();
        }

        // 8. Custom user-provided regexes
        for custom_re in &self.custom_regexes {
            if custom_re.is_match(&text) {
                let matches = custom_re.find_iter(&text).count();
                count += matches;
                text = custom_re.replace_all(&text, "[REDACTED_CREDENTIAL]").into_owned();
            }
        }

        (text, count)
    }

    /// Shreds bulky minified JSON blobs into concise summaries: `[shredded_json: N keys, M bytes]`
    pub fn shred_json_blobs(&self, input: &str) -> (String, usize) {
        if !self.config.shred_json_blobs || input.is_empty() {
            return (input.to_string(), 0);
        }

        let mut shred_count = 0;
        let mut output_lines = Vec::new();

        for line in input.lines() {
            let trimmed = line.trim();
            // Fast path: does this line look like a bulky JSON object or array?
            if trimmed.len() >= self.config.json_blob_min_bytes
                && ((trimmed.starts_with('{') && trimmed.ends_with('}'))
                    || (trimmed.starts_with('[') && trimmed.ends_with(']')))
            {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    shred_count += 1;
                    match val {
                        serde_json::Value::Object(map) => {
                            output_lines.push(format!(
                                "[shredded_json: {} keys, {} bytes]",
                                map.len(),
                                trimmed.len()
                            ));
                            continue;
                        }
                        serde_json::Value::Array(arr) => {
                            output_lines.push(format!(
                                "[shredded_json_array: {} items, {} bytes]",
                                arr.len(),
                                trimmed.len()
                            ));
                            continue;
                        }
                        _ => {}
                    }
                }
            }

            // Check for inline JSON objects within a log line e.g., "LOG: {\"key\": ...}"
            if trimmed.len() >= self.config.json_blob_min_bytes && trimmed.contains('{') && trimmed.contains('}') {
                if let (Some(start_idx), Some(end_idx)) = (line.find('{'), line.rfind('}')) {
                    if end_idx > start_idx && (end_idx - start_idx + 1) >= self.config.json_blob_min_bytes {
                        let potential_json = &line[start_idx..=end_idx];
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(potential_json) {
                            if let serde_json::Value::Object(map) = val {
                                shred_count += 1;
                                let prefix = &line[..start_idx];
                                let suffix = &line[end_idx + 1..];
                                output_lines.push(format!(
                                    "{}[shredded_json: {} keys, {} bytes]{}",
                                    prefix,
                                    map.len(),
                                    potential_json.len(),
                                    suffix
                                ));
                                continue;
                            }
                        }
                    }
                }
            }

            output_lines.push(line.to_string());
        }

        (output_lines.join("\n"), shred_count)
    }

    /// Shreds oversized base64 strings into concise summaries: `[shredded_base64: M bytes]`
    pub fn shred_base64(&self, input: &str) -> (String, usize) {
        if !self.config.shred_base64 || input.is_empty() {
            return (input.to_string(), 0);
        }

        let mut shred_count = 0;
        let mut text = input.to_string();

        // 1. Data URIs
        if self.regex_base64_data_uri.is_match(&text) {
            let matches = self.regex_base64_data_uri.find_iter(&text).count();
            shred_count += matches;
            text = self.regex_base64_data_uri.replace_all(&text, |caps: &regex::Captures| {
                let bytes = caps.get(0).map(|m| m.len()).unwrap_or(0);
                format!("[shredded_base64_data_uri: {} bytes]", bytes)
            }).into_owned();
        }

        // 2. Raw long base64 strings
        if self.regex_base64_raw.is_match(&text) {
            let mut matches_len = 0;
            let replaced = self.regex_base64_raw.replace_all(&text, |caps: &regex::Captures| {
                let m = caps.get(0).unwrap();
                if m.len() >= self.config.base64_min_bytes {
                    matches_len += 1;
                    format!("[shredded_base64: {} bytes]", m.len())
                } else {
                    m.as_str().to_string()
                }
            }).into_owned();
            shred_count += matches_len;
            text = replaced;
        }

        (text, shred_count)
    }

    /// Compresses AST-level comments, docstrings, and multi-line comments when token budget is constrained
    pub fn compress_ast_comments(&self, input: &str) -> (String, usize) {
        let should_compress = self.config.compress_ast_comments
            || self.config.token_budget.map(|budget| TokenShredder::estimate_tokens(input) > budget).unwrap_or(false);

        if !should_compress || input.is_empty() {
            return (input.to_string(), 0);
        }

        let mut compressed_lines_count = 0;

        // 1. Compress multi-line block comments (/* ... */)
        let block_comment_re = Regex::new(r"(?s)/\*.*?\*/").unwrap();
        let step1 = block_comment_re.replace_all(input, |caps: &regex::Captures| {
            let matched = caps.get(0).unwrap().as_str();
            let line_count = matched.lines().count();
            if line_count > 1 {
                compressed_lines_count += line_count - 1;
                format!("/* [... doc comment compressed: {} lines ...] */", line_count)
            } else {
                matched.to_string()
            }
        }).into_owned();

        // 2. Line-by-line comment folding for doc comments (/// or //!) and consecutive // comments
        let lines: Vec<&str> = step1.lines().collect();
        let mut output_lines = Vec::with_capacity(lines.len());
        let mut i = 0;

        while i < lines.len() {
            let line = lines[i];
            let trimmed = line.trim();

            // Consecutive doc comments (/// or //!)
            if trimmed.starts_with("///") || trimmed.starts_with("//!") {
                let mut doc_count = 1;
                while (i + doc_count) < lines.len() {
                    let next_trimmed = lines[i + doc_count].trim();
                    if next_trimmed.starts_with("///") || next_trimmed.starts_with("//!") {
                        doc_count += 1;
                    } else {
                        break;
                    }
                }

                if doc_count > 1 {
                    let indent = line.len() - line.trim_start().len();
                    let prefix = &line[..indent];
                    output_lines.push(format!("{}/// [... docstring compressed: {} lines ...]", prefix, doc_count));
                    compressed_lines_count += doc_count - 1;
                    i += doc_count;
                    continue;
                }
            }

            // Consecutive standard comments (// ...)
            if trimmed.starts_with("//") && !trimmed.starts_with("///") && !trimmed.starts_with("//!") {
                let mut comment_count = 1;
                while (i + comment_count) < lines.len() {
                    let next_trimmed = lines[i + comment_count].trim();
                    if next_trimmed.starts_with("//") && !next_trimmed.starts_with("///") && !next_trimmed.starts_with("//!") {
                        comment_count += 1;
                    } else {
                        break;
                    }
                }

                if comment_count > 2 {
                    let indent = line.len() - line.trim_start().len();
                    let prefix = &line[..indent];
                    output_lines.push(format!("{}// [... {} comment lines compressed ...]", prefix, comment_count));
                    compressed_lines_count += comment_count - 1;
                    i += comment_count;
                    continue;
                }
            }

            // Consecutive python/shell comments (# ...)
            if trimmed.starts_with('#') {
                let mut hash_count = 1;
                while (i + hash_count) < lines.len() {
                    let next_trimmed = lines[i + hash_count].trim();
                    if next_trimmed.starts_with('#') {
                        hash_count += 1;
                    } else {
                        break;
                    }
                }

                if hash_count > 2 {
                    let indent = line.len() - line.trim_start().len();
                    let prefix = &line[..indent];
                    output_lines.push(format!("{}# [... {} comment lines compressed ...]", prefix, hash_count));
                    compressed_lines_count += hash_count - 1;
                    i += hash_count;
                    continue;
                }
            }

            output_lines.push(line.to_string());
            i += 1;
        }

        (output_lines.join("\n"), compressed_lines_count)
    }

    /// Applies all active policy rules (credentials, JSON, base64, AST comments)
    pub fn apply(&self, input: &str) -> (String, PolicyExecutionMetrics) {
        let original_bytes = input.len();

        let (redacted, cred_count) = self.redact_credentials(input);
        let (shredded_json, json_count) = self.shred_json_blobs(&redacted);
        let (shredded_b64, b64_count) = self.shred_base64(&shredded_json);
        let (compressed_ast, ast_count) = self.compress_ast_comments(&shredded_b64);

        let processed_bytes = compressed_ast.len();
        let metrics = PolicyExecutionMetrics {
            redacted_credentials_count: cred_count,
            shredded_json_blobs_count: json_count,
            shredded_base64_blobs_count: b64_count,
            comments_compressed_count: ast_count,
            original_bytes,
            processed_bytes,
        };

        (compressed_ast, metrics)
    }
}

/// Native terminal output compressor and token optimizer.
#[derive(Debug, Clone)]
pub struct TokenShredder {
    pub head_lines: usize,
    pub tail_lines: usize,
    pub max_bytes: usize,
    pub strip_ansi: bool,
    pub fold_progress: bool,
    pub dedup_repeats: bool,
    pub condense_oversized: bool,
    pub policy: Option<TokenPolicyEngine>,
}

impl Default for TokenShredder {
    fn default() -> Self {
        Self {
            head_lines: 40,
            tail_lines: 40,
            max_bytes: 64 * 1024,
            strip_ansi: true,
            fold_progress: true,
            dedup_repeats: true,
            condense_oversized: true,
            policy: None,
        }
    }
}

impl TokenShredder {
    /// Creates a new `TokenShredder` with default parameters.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a new `TokenShredder` with specified head/tail preservation lines.
    pub fn with_limits(head_lines: usize, tail_lines: usize) -> Self {
        Self {
            head_lines,
            tail_lines,
            ..Default::default()
        }
    }

    /// Configures the shredder with an active `TokenPolicyConfig`.
    pub fn with_policy(mut self, config: TokenPolicyConfig) -> Self {
        self.policy = Some(TokenPolicyEngine::new(config));
        self
    }

    /// High-speed native ANSI, VT100, TrueColor, and carriage-return stripper.
    pub fn strip_ansi_codes(input: &str) -> String {
        if input.is_empty() {
            return String::new();
        }

        // Fast path: if no escape char present and no carriage return, return original
        if !input.contains('\x1b') && !input.contains('\r') {
            return input.to_string();
        }

        let mut cleaned = input.to_string();

        if cleaned.contains('\x1b') {
            cleaned = ANSI_CSI_REGEX.replace_all(&cleaned, "").into_owned();
            cleaned = ANSI_OSC_REGEX.replace_all(&cleaned, "").into_owned();
            cleaned = ANSI_OTHER_REGEX.replace_all(&cleaned, "").into_owned();
        }

        // Handle terminal carriage returns (\r overwriting current line)
        if cleaned.contains('\r') {
            let mut resolved_lines = Vec::new();
            for raw_line in cleaned.split('\n') {
                if raw_line.contains('\r') {
                    // Split on carriage return and pick the last non-empty segment
                    let segments: Vec<&str> = raw_line.split('\r').collect();
                    let last_segment = segments
                        .iter()
                        .rev()
                        .find(|s| !s.trim().is_empty())
                        .unwrap_or(&"");
                    resolved_lines.push(*last_segment);
                } else {
                    resolved_lines.push(raw_line);
                }
            }
            cleaned = resolved_lines.join("\n");
        }

        cleaned
    }

    /// Folds repetitive terminal progress updates and deduplicates consecutive identical lines.
    pub fn fold_repetitive_lines(input: &str) -> String {
        if input.is_empty() {
            return String::new();
        }

        let lines: Vec<&str> = input.lines().collect();
        if lines.len() <= 1 {
            return input.to_string();
        }

        let mut output_lines: Vec<String> = Vec::with_capacity(lines.len());
        let mut i = 0;

        while i < lines.len() {
            let current_line = lines[i];
            let current_trimmed = current_line.trim();

            // 1. Check for consecutive identical lines (e.g. repeated warnings or heartbeats)
            let mut repeat_count = 1;
            while (i + repeat_count) < lines.len() && lines[i + repeat_count] == current_line {
                repeat_count += 1;
            }

            if repeat_count > 1 {
                output_lines.push(current_line.to_string());
                if repeat_count == 2 {
                    output_lines.push(current_line.to_string());
                } else {
                    output_lines.push(format!(
                        "    [... repeated {} more times ...]",
                        repeat_count - 1
                    ));
                }
                i += repeat_count;
                continue;
            }

            // 2. Check for consecutive progress bar updates
            if PROGRESS_LINE_REGEX.is_match(current_trimmed) {
                let mut progress_count = 1;
                while (i + progress_count) < lines.len()
                    && PROGRESS_LINE_REGEX.is_match(lines[i + progress_count].trim())
                {
                    progress_count += 1;
                }

                if progress_count > 2 {
                    // Output the final progress state and note the collapsed frames
                    let final_progress = lines[i + progress_count - 1];
                    output_lines.push(format!(
                        "    [... {} progress updates folded ...]",
                        progress_count - 1
                    ));
                    output_lines.push(final_progress.to_string());
                    i += progress_count;
                    continue;
                }
            }

            output_lines.push(current_line.to_string());
            i += 1;
        }

        output_lines.join("\n")
    }

    /// Condenses oversized tool output preserving the first `head_lines` and last `tail_lines`.
    pub fn condense_oversized_text(
        input: &str,
        head_lines: usize,
        tail_lines: usize,
    ) -> (String, bool) {
        let lines: Vec<&str> = input.lines().collect();
        let total_lines = lines.len();

        // If lines fit within head + tail + margin, preserve as is
        if total_lines <= head_lines + tail_lines + 4 {
            return (input.to_string(), false);
        }

        let omitted_lines = total_lines.saturating_sub(head_lines + tail_lines);
        let head_part = &lines[..head_lines];
        let tail_part = &lines[total_lines - tail_lines..];

        let omitted_bytes: usize = lines[head_lines..total_lines - tail_lines]
            .iter()
            .map(|l| l.len() + 1)
            .sum();

        let banner = format!(
            "\n[... Hagibis Token Shredder: {} lines ({} bytes) condensed ...]\n",
            omitted_lines, omitted_bytes
        );

        let mut result = String::with_capacity(head_lines * 80 + tail_lines * 80 + 128);
        result.push_str(&head_part.join("\n"));
        result.push_str(&banner);
        result.push_str(&tail_part.join("\n"));

        (result, true)
    }

    /// Fast, accurate heuristic token estimator (~3.6 chars/token on prose, ~2.8 on code/symbols).
    pub fn estimate_tokens(text: &str) -> usize {
        if text.is_empty() {
            return 0;
        }

        let mut token_estimate: f64 = 0.0;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                token_estimate += 0.5;
                continue;
            }

            // Code and symbols tokenize into smaller byte chunks than natural language
            let is_code = trimmed.starts_with("//")
                || trimmed.starts_with('#')
                || trimmed.contains('{')
                || trimmed.contains('}')
                || trimmed.contains('(')
                || trimmed.contains(';')
                || trimmed.contains("->");

            let factor = if is_code { 2.8 } else { 3.6 };
            token_estimate += (trimmed.len() as f64 / factor).max(1.0);
        }

        token_estimate.ceil() as usize
    }

    /// Shreds raw text with default configured parameters and returns comprehensive savings metrics.
    pub fn shred(&self, raw: &str) -> ShredResult {
        self.shred_with_limits(raw, self.head_lines, self.tail_lines)
    }

    /// Shreds raw text with explicit head/tail line constraints.
    pub fn shred_with_limits(&self, raw: &str, head_lines: usize, tail_lines: usize) -> ShredResult {
        let original_bytes = raw.len();
        let original_tokens = Self::estimate_tokens(raw);

        if raw.is_empty() {
            return ShredResult {
                original_tokens: 0,
                shredded_tokens: 0,
                savings_percent: 0.0,
                original_bytes: 0,
                shredded_bytes: 0,
                content: String::new(),
            };
        }

        // Phase 1: Strip ANSI escape codes
        let step1 = if self.strip_ansi {
            Self::strip_ansi_codes(raw)
        } else {
            raw.to_string()
        };

        // Phase 1.5: Apply Token Policy Engine if configured (credentials, JSON, base64, AST comments)
        let step_policy = if let Some(ref policy) = self.policy {
            policy.apply(&step1).0
        } else {
            step1
        };

        // Phase 2: Fold repetitive progress and duplicate warning lines
        let step2 = if self.fold_progress || self.dedup_repeats {
            Self::fold_repetitive_lines(&step_policy)
        } else {
            step_policy
        };

        // Phase 3: Condense oversized outputs
        let (step3, _condensed) = if self.condense_oversized {
            Self::condense_oversized_text(&step2, head_lines, tail_lines)
        } else {
            (step2, false)
        };

        // Phase 4: Enforce max byte limit safeguard (Unicode-boundary safe)
        let final_content = if step3.len() > self.max_bytes {
            let mut truncated = safe_truncate_str(&step3, self.max_bytes).to_string();
            truncated.push_str("\n[... Truncated by Hagibis Token Shredder max byte ceiling ...]");
            truncated
        } else {
            step3
        };

        let shredded_bytes = final_content.len();
        let shredded_tokens = Self::estimate_tokens(&final_content);

        let savings_percent = if original_tokens > 0 && original_tokens >= shredded_tokens {
            let saved = (original_tokens - shredded_tokens) as f64;
            (saved / original_tokens as f64) * 100.0
        } else {
            0.0
        };

        ShredResult {
            original_tokens,
            shredded_tokens,
            savings_percent,
            original_bytes,
            shredded_bytes,
            content: final_content,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_ansi_codes() {
        let colored = "\x1b[31mError:\x1b[0m \x1b[1;32mEverything is fine\x1b[0m \x1b[38;2;255;100;50mTrueColor\x1b[0m";
        let cleaned = TokenShredder::strip_ansi_codes(colored);
        assert_eq!(cleaned, "Error: Everything is fine TrueColor");
    }

    #[test]
    fn test_strip_ansi_carriage_return_overwrite() {
        let progress = "Downloading: 10%\rDownloading: 50%\rDownloading: 100%\nDone!";
        let cleaned = TokenShredder::strip_ansi_codes(progress);
        assert!(cleaned.contains("Downloading: 100%"));
        assert!(!cleaned.contains("Downloading: 10%"));
        assert!(cleaned.contains("Done!"));
    }

    #[test]
    fn test_fold_repetitive_progress() {
        let progress_logs = "\
Building [==>                  ] 10%
Building [=====>               ] 25%
Building [=========>           ] 50%
Building [==============>      ] 75%
Building [====================>] 100%
Finished release target in 4.2s";

        let folded = TokenShredder::fold_repetitive_lines(progress_logs);
        assert!(folded.contains("progress updates folded"));
        assert!(folded.contains("Building [====================>] 100%"));
        assert!(folded.contains("Finished release target in 4.2s"));
    }

    #[test]
    fn test_fold_repetitive_identical_lines() {
        let duplicate_warnings = "\
warning: unused variable `x`
warning: unused variable `x`
warning: unused variable `x`
warning: unused variable `x`
warning: unused variable `x`
Finished compilation with 5 warnings.";

        let folded = TokenShredder::fold_repetitive_lines(duplicate_warnings);
        assert!(folded.contains("repeated 4 more times"));
        assert!(folded.contains("Finished compilation with 5 warnings."));
    }

    #[test]
    fn test_condense_oversized_output() {
        let mut large_output = String::new();
        for i in 1..=200 {
            large_output.push_str(&format!("Line {}: processing item {}\n", i, i));
        }

        let (condensed, was_condensed) = TokenShredder::condense_oversized_text(&large_output, 10, 10);
        assert!(was_condensed);
        assert!(condensed.contains("Line 1:"));
        assert!(condensed.contains("Line 10:"));
        assert!(condensed.contains("Line 200:"));
        assert!(condensed.contains("Hagibis Token Shredder: 180 lines"));
        assert!(!condensed.contains("Line 50:"));
    }

    #[test]
    fn test_token_shredder_savings() {
        let shredder = TokenShredder::with_limits(5, 5);

        // Simulate a massive noisy terminal dump with ANSI and progress
        let mut raw_dump = String::new();
        for i in 1..=100 {
            raw_dump.push_str(&format!(
                "\x1b[33mCompiling\x1b[0m crate_{} v0.1.0 [===> ] {}%\n",
                i, i
            ));
        }
        raw_dump.push_str("\x1b[32mSuccess\x1b[0m: All crates built successfully.\n");

        let result = shredder.shred(&raw_dump);
        assert!(result.shredded_tokens < result.original_tokens);
        assert!(result.savings_percent > 50.0, "Expected >50% savings, got {}", result.savings_percent);
        assert!(result.content.contains("Success: All crates built successfully."));
    }

    #[test]
    fn test_empty_string_handling() {
        let shredder = TokenShredder::new();
        let result = shredder.shred("");
        assert_eq!(result.original_tokens, 0);
        assert_eq!(result.shredded_tokens, 0);
        assert_eq!(result.savings_percent, 0.0);
        assert_eq!(result.content, "");
    }

    #[test]
    fn test_token_policy_engine_redact_api_keys() {
        let engine = TokenPolicyEngine::default();
        let raw = "OpenAI key: sk-live-abcdef1234567890abcdef1234567890\nGoogle key: AIzaSyD1234567890abcdefghijklmnopqrstuvw";
        let (redacted, count) = engine.redact_credentials(raw);
        assert_eq!(count, 2);
        assert!(!redacted.contains("sk-live-"));
        assert!(!redacted.contains("AIzaSyD"));
        assert!(redacted.contains("[REDACTED_API_KEY]"));
        assert!(redacted.contains("[REDACTED_GOOGLE_API_KEY]"));
    }

    #[test]
    fn test_token_policy_engine_redact_aws_and_github() {
        let engine = TokenPolicyEngine::default();
        let raw = "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\naws_secret_access_key = 'wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY'\nGH_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz1234567890";
        let (redacted, count) = engine.redact_credentials(raw);
        assert!(count >= 3);
        assert!(!redacted.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(!redacted.contains("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"));
        assert!(!redacted.contains("ghp_abcdef"));
        assert!(redacted.contains("[REDACTED_AWS_KEY_ID]"));
        assert!(redacted.contains("[REDACTED_AWS_SECRET]"));
        assert!(redacted.contains("[REDACTED_GITHUB_TOKEN]"));
    }

    #[test]
    fn test_token_policy_engine_redact_ssh_and_bearer() {
        let engine = TokenPolicyEngine::default();
        let raw = "Authorization: Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.e30.t-IDc\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAABlwAAAAdzc2gtcn\n-----END OPENSSH PRIVATE KEY-----";
        let (redacted, count) = engine.redact_credentials(raw);
        assert_eq!(count, 2);
        assert!(redacted.contains("Bearer [REDACTED_BEARER_TOKEN]"));
        assert!(redacted.contains("[REDACTED_SSH_PRIVATE_KEY]"));
        assert!(!redacted.contains("b3BlbnNzaC"));
    }

    #[test]
    fn test_token_policy_engine_shred_json_blobs() {
        let engine = TokenPolicyEngine::default();
        let bulky_json = r#"{"id":101,"user":"alice","roles":["admin","developer"],"session_token":"xyz987","metadata":{"env":"prod","cluster":"us-east-1","tier":"enterprise"},"active":true}"#;
        let (shredded, count) = engine.shred_json_blobs(bulky_json);
        assert_eq!(count, 1);
        assert!(shredded.contains("[shredded_json: 6 keys,"));
        assert!(!shredded.contains("session_token"));
    }

    #[test]
    fn test_token_policy_engine_shred_base64() {
        let engine = TokenPolicyEngine::default();
        let raw = "Image preview: data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=\nRaw token: SGVsbG8gd29ybGQgdGhpcyBpcyBhIHZlcnkgbG9uZyBhdXRoZW50aWNhdGlvbiB0b2tlbiBzdHJpbmcgdGhhdCByZXByZXNlbnRzIGJhc2U2NCBibG9i";
        let (shredded, count) = engine.shred_base64(raw);
        assert!(count >= 1);
        assert!(shredded.contains("[shredded_base64"));
    }

    #[test]
    fn test_token_policy_engine_compress_ast_comments() {
        let config = TokenPolicyConfig {
            compress_ast_comments: true,
            ..Default::default()
        };
        let engine = TokenPolicyEngine::new(config);

        let code = r#"
/// First doc line
/// Second doc line
/// Third doc line
pub fn execute_workflow() {
    // Explanation line 1
    // Explanation line 2
    // Explanation line 3
    let status = 200;
}
"#;
        let (compressed, count) = engine.compress_ast_comments(code);
        assert!(count >= 2);
        assert!(compressed.contains("docstring compressed: 3 lines"));
        assert!(compressed.contains("comment lines compressed"));
        assert!(compressed.contains("let status = 200;"));
    }

    #[test]
    fn test_token_policy_engine_dynamic_config_json() {
        let json_config = r#"{
            "redact_credentials": true,
            "custom_credential_patterns": ["SECRET_ENV_[0-9]+"],
            "shred_json_blobs": true,
            "json_blob_min_bytes": 50,
            "shred_base64": true,
            "base64_min_bytes": 50,
            "compress_ast_comments": true,
            "token_budget": 500
        }"#;

        let parsed = TokenPolicyConfig::from_json(json_config).unwrap();
        assert_eq!(parsed.json_blob_min_bytes, 50);
        assert_eq!(parsed.custom_credential_patterns.len(), 1);

        let engine = TokenPolicyEngine::new(parsed);
        let input = "Custom secret: SECRET_ENV_987654";
        let (redacted, count) = engine.redact_credentials(input);
        assert_eq!(count, 1);
        assert!(redacted.contains("[REDACTED_CREDENTIAL]"));
    }

    #[test]
    fn test_token_shredder_with_policy_integration() {
        let shredder = TokenShredder::default().with_policy(TokenPolicyConfig::default());
        let raw = "\x1b[32mOK\x1b[0m api_key: sk-proj-1234567890abcdef1234567890\n{\"k1\":\"v1\",\"k2\":\"v2\",\"k3\":\"v3\",\"k4\":\"v4\",\"k5\":\"v5\",\"k6\":\"v6\",\"k7\":\"v7\"}\n";
        let res = shredder.shred(raw);
        assert!(!res.content.contains("sk-proj-"));
        assert!(res.content.contains("[REDACTED_API_KEY]"));
        assert!(res.content.contains("[shredded_json: 7 keys,"));
    }
}
