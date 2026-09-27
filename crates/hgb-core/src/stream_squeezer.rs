//! # StreamSqueezer - High-Signal Terminal Stream & Log Compressor
//!
//! Elevates Claude Code's terminal output interceptor. Squeezes massive, noisy
//! terminal outputs (compiler dumps, npm builds, test runner logs) into
//! dense, actionable diagnostic digests that fit within strict LLM token budgets.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Structured digest of squeezed terminal telemetry
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SqueezedDigest {
    pub total_raw_lines: usize,
    pub squeezed_lines: usize,
    pub compression_ratio_pct: f64,
    pub errors: Vec<String>,
    pub deduplicated_warnings: Vec<String>,
    pub stack_trace: Option<String>,
    pub file_locations: Vec<String>,
    pub compressed_view: String,
}

pub struct StreamSqueezer;

impl StreamSqueezer {
    /// Squeeze noisy terminal output into high-signal diagnostic digest
    pub fn squeeze(raw_output: &str, max_tokens: usize) -> SqueezedDigest {
        let clean = Self::strip_ansi_codes(raw_output);
        let raw_lines: Vec<&str> = clean.lines().collect();
        let total_raw_lines = raw_lines.len();

        let mut errors = Vec::new();
        let mut warnings_map: HashMap<String, usize> = HashMap::new();
        let mut stack_frames = Vec::new();
        let mut file_locations = Vec::new();

        let file_loc_re = Regex::new(r"([a-zA-Z0-9_\-\./\\]+\.[a-zA-Z0-9_]+):(\d+)(?::(\d+))?").unwrap();
        let is_progress_re = Regex::new(r"(?:\d+/\d+|\d+%|\[=+>*\s*\]|Downloading|Fetching|\.{3,})").unwrap();

        for line in &raw_lines {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Skip pure progress bar noise
            if is_progress_re.is_match(trimmed) && trimmed.len() < 80 && !trimmed.contains("error") {
                continue;
            }

            // Extract file:line:col citations
            if let Some(caps) = file_loc_re.captures(trimmed) {
                let loc = caps[0].to_string();
                if !file_locations.contains(&loc) && file_locations.len() < 25 {
                    file_locations.push(loc);
                }
            }

            // Categorize
            if trimmed.contains("error:")
                || trimmed.contains("error[E")
                || trimmed.contains("SyntaxError:")
                || trimmed.contains("TypeError:")
                || trimmed.contains("FATAL")
                || trimmed.contains("panicked at")
            {
                if errors.len() < 50 {
                    errors.push(trimmed.to_string());
                }
            } else if trimmed.contains("warning:") || trimmed.contains("WARN") {
                // Normalize warning to base message for deduplication
                let key = if let Some(idx) = trimmed.find("warning:") {
                    trimmed[idx..].chars().take(80).collect::<String>()
                } else {
                    trimmed.chars().take(80).collect::<String>()
                };
                *warnings_map.entry(key).or_insert(0) += 1;
            } else if trimmed.starts_with("at ")
                || trimmed.starts_with("-->")
                || trimmed.contains("stack backtrace:")
                || trimmed.contains("Traceback (most recent call last)")
            {
                if stack_frames.len() < 30 {
                    stack_frames.push(trimmed.to_string());
                }
            }
        }

        let mut deduplicated_warnings = Vec::new();
        for (warn, count) in warnings_map {
            if count > 1 {
                deduplicated_warnings.push(format!("{} (repeated {}x)", warn, count));
            } else {
                deduplicated_warnings.push(warn);
            }
        }
        deduplicated_warnings.sort();

        let stack_trace = if !stack_frames.is_empty() {
            Some(stack_frames.join("\n"))
        } else {
            None
        };

        // Render compressed view fitting within token budget (~4 chars per token)
        let max_chars = max_tokens * 4;
        let mut compressed_view = String::new();
        compressed_view.push_str("=== SQUEEZED TERMINAL TELEMETRY ===\n");

        if !errors.is_empty() {
            compressed_view.push_str(&format!("[ERRORS - {} found]\n", errors.len()));
            for err in &errors {
                let err_line = format!("  ❌ {}\n", err);
                if compressed_view.len() + err_line.len() > max_chars {
                    break;
                }
                compressed_view.push_str(&err_line);
            }
        }

        if let Some(ref st) = stack_trace {
            compressed_view.push_str("\n[RELEVANT STACK FRAMES]\n");
            for frame in st.lines().take(15) {
                let frame_line = format!("  ⚡ {}\n", frame);
                if compressed_view.len() + frame_line.len() > max_chars {
                    break;
                }
                compressed_view.push_str(&frame_line);
            }
        }

        if !file_locations.is_empty() {
            compressed_view.push_str("\n[CODE LOCATIONS TO INSPECT]\n");
            for loc in &file_locations {
                let loc_line = format!("  📍 {}\n", loc);
                if compressed_view.len() + loc_line.len() > max_chars {
                    break;
                }
                compressed_view.push_str(&loc_line);
            }
        }

        if !deduplicated_warnings.is_empty() {
            compressed_view.push_str(&format!("\n[WARNINGS SUMMARY - {} unique]\n", deduplicated_warnings.len()));
            for warn in deduplicated_warnings.iter().take(10) {
                let w_line = format!("  ⚠️ {}\n", warn);
                if compressed_view.len() + w_line.len() > max_chars {
                    break;
                }
                compressed_view.push_str(&w_line);
            }
        }

        let squeezed_lines = compressed_view.lines().count();
        let compression_ratio_pct = if total_raw_lines > 0 {
            (1.0 - (squeezed_lines as f64 / total_raw_lines as f64)) * 100.0
        } else {
            0.0
        };

        SqueezedDigest {
            total_raw_lines,
            squeezed_lines,
            compression_ratio_pct,
            errors,
            deduplicated_warnings,
            stack_trace,
            file_locations,
            compressed_view,
        }
    }

    /// Strip ANSI escape codes (colors, cursor movements, erase line)
    pub fn strip_ansi_codes(input: &str) -> String {
        let ansi_re = Regex::new(r"\x1B(?:[@-Z\\-_]|\[[0-?]*[ -/]*[@-~])").unwrap();
        ansi_re.replace_all(input, "").to_string()
    }
}
