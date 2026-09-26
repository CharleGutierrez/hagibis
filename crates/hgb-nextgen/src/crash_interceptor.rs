//! # Crash Interceptor & "Zero-Click Heal" Prompt
//!
//! Autonomous runtime error monitor that intercepts:
//! - Rust compiler errors (`rustc`, `cargo check`, `cargo test`)
//! - Python tracebacks (`Traceback (most recent call last)`)
//! - Unhandled panics (`thread 'main' panicked at file:line`)
//! - Node.js / JavaScript exceptions (`TypeError`, `ReferenceError`, stack traces)
//!
//! Formats actionable high-visibility banners with clickable/keyboard 1-Click Heal targets.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Origin language / runtime of intercepted crash
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CrashSourceType {
    Rustc,
    Cargo,
    PythonTraceback,
    UnhandledPanic,
    NodeCrash,
}

impl CrashSourceType {
    pub fn badge(&self) -> &'static str {
        match self {
            Self::Rustc => "RUSTC",
            Self::Cargo => "CARGO",
            Self::PythonTraceback => "PYTHON",
            Self::UnhandledPanic => "PANIC",
            Self::NodeCrash => "NODE",
        }
    }
}

/// An intercepted crash diagnostic with pinpointed location and error semantics
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterceptedCrash {
    pub source_type: CrashSourceType,
    pub file_path: PathBuf,
    pub line_number: usize,
    pub column_number: Option<usize>,
    pub error_type: String,
    pub error_message: String,
    pub stack_snippet: Option<String>,
}

impl InterceptedCrash {
    /// Format into the standard high-visibility Crash Interceptor banner string:
    /// `⚠️ CRASH INTERCEPTOR: <file>:<line> (<error>) [🚑 1-Click Heal]`
    pub fn banner_text(&self) -> String {
        format!(
            "⚠️ CRASH INTERCEPTOR: {}:{} ({}: {}) [🚑 1-Click Heal]",
            self.file_path.display(),
            self.line_number,
            self.error_type,
            self.error_message
        )
    }

    /// Generate automated prompt for self-healing engine or agent
    pub fn heal_prompt(&self) -> String {
        format!(
            "/heal Fix {} error in {}:{} - {}: {}",
            self.source_type.badge(),
            self.file_path.display(),
            self.line_number,
            self.error_type,
            self.error_message
        )
    }
}

/// Autonomous Crash Detector
pub struct CrashDetector;

impl CrashDetector {
    /// Detect first crash in command/test/devserver stdout or stderr
    pub fn detect(output: &str) -> Option<InterceptedCrash> {
        Self::detect_all(output).into_iter().next()
    }

    /// Scan and extract all crashes from execution output
    pub fn detect_all(output: &str) -> Vec<InterceptedCrash> {
        let mut results = Vec::new();

        // 1. Unhandled Rust Panic: `thread 'main' panicked at src/lib.rs:88:5:\nassertion failed: ...`
        let panic_re = Regex::new(r"thread '.*?' panicked at (.*?):(\d+):(\d+):\n?(.*)").unwrap();
        for cap in panic_re.captures_iter(output) {
            let file = cap.get(1).map(|m| m.as_str().trim()).unwrap_or("unknown");
            let line = cap.get(2).and_then(|m| m.as_str().parse::<usize>().ok()).unwrap_or(1);
            let col = cap.get(3).and_then(|m| m.as_str().parse::<usize>().ok());
            let msg = cap.get(4).map(|m| m.as_str().trim()).unwrap_or("explicit panic");

            results.push(InterceptedCrash {
                source_type: CrashSourceType::UnhandledPanic,
                file_path: PathBuf::from(file),
                line_number: line,
                column_number: col,
                error_type: "Panic".to_string(),
                error_message: msg.to_string(),
                stack_snippet: Some(cap.get(0).map(|m| m.as_str().to_string()).unwrap_or_default()),
            });
        }

        // 2. Rustc Compiler Diagnostic: `error[E0308]: mismatched types\n  --> src/main.rs:42:15`
        let rustc_re = Regex::new(r"(?m)error(?:\[([A-Z0-9]+)\])?: (.*?)\n\s+-->\s+(.*?):(\d+):(\d+)").unwrap();
        for cap in rustc_re.captures_iter(output) {
            let code = cap.get(1).map(|m| m.as_str().to_string()).unwrap_or_else(|| "Error".to_string());
            let msg = cap.get(2).map(|m| m.as_str().trim()).unwrap_or("");
            let file = cap.get(3).map(|m| m.as_str().trim()).unwrap_or("unknown");
            let line = cap.get(4).and_then(|m| m.as_str().parse::<usize>().ok()).unwrap_or(1);
            let col = cap.get(5).and_then(|m| m.as_str().parse::<usize>().ok());

            results.push(InterceptedCrash {
                source_type: CrashSourceType::Rustc,
                file_path: PathBuf::from(file),
                line_number: line,
                column_number: col,
                error_type: code,
                error_message: msg.to_string(),
                stack_snippet: Some(cap.get(0).map(|m| m.as_str().to_string()).unwrap_or_default()),
            });
        }

        // 3. Python Traceback:
        // Traceback (most recent call last):
        //   File "server.py", line 42, in <module>
        // ZeroDivisionError: division by zero
        let py_re = Regex::new(r#"(?s)Traceback \(most recent call last\):.*?File "([^"]+)", line (\d+)(?:, in [^\n]+)?\n(?:\s+.*?\n)?([A-Za-z0-9_]+Error|[A-Za-z0-9_]+Exception): (.*?)(?:\n|$)"#).unwrap();
        for cap in py_re.captures_iter(output) {
            let file = cap.get(1).map(|m| m.as_str().trim()).unwrap_or("unknown");
            let line = cap.get(2).and_then(|m| m.as_str().parse::<usize>().ok()).unwrap_or(1);
            let err_type = cap.get(3).map(|m| m.as_str().trim()).unwrap_or("PythonError");
            let msg = cap.get(4).map(|m| m.as_str().trim()).unwrap_or("");

            results.push(InterceptedCrash {
                source_type: CrashSourceType::PythonTraceback,
                file_path: PathBuf::from(file),
                line_number: line,
                column_number: None,
                error_type: err_type.to_string(),
                error_message: msg.to_string(),
                stack_snippet: Some(cap.get(0).map(|m| m.as_str().to_string()).unwrap_or_default()),
            });
        }

        // 4. Node.js / V8 Exception:
        // TypeError: Cannot read properties of undefined (reading 'map')
        //     at /app/src/index.js:42:15
        let node_re = Regex::new(r"(?m)^([A-Za-z0-9_]+Error): (.*?)\n\s+at (?:.*? \()?([^:\(\)\n]+):(\d+):(\d+)\)?").unwrap();
        for cap in node_re.captures_iter(output) {
            let err_type = cap.get(1).map(|m| m.as_str().trim()).unwrap_or("NodeError");
            let msg = cap.get(2).map(|m| m.as_str().trim()).unwrap_or("");
            let file = cap.get(3).map(|m| m.as_str().trim()).unwrap_or("unknown");
            let line = cap.get(4).and_then(|m| m.as_str().parse::<usize>().ok()).unwrap_or(1);
            let col = cap.get(5).and_then(|m| m.as_str().parse::<usize>().ok());

            results.push(InterceptedCrash {
                source_type: CrashSourceType::NodeCrash,
                file_path: PathBuf::from(file),
                line_number: line,
                column_number: col,
                error_type: err_type.to_string(),
                error_message: msg.to_string(),
                stack_snippet: Some(cap.get(0).map(|m| m.as_str().to_string()).unwrap_or_default()),
            });
        }

        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rustc_error_interception() {
        let output = r#"
Compiling hagibis v0.1.0 (/workspace)
error[E0308]: mismatched types
  --> src/cockpit.rs:142:25
   |
142 |     let x: u32 = "hello";
   |                  ^^^^^^^ expected `u32`, found `&str`
"#;
        let crash = CrashDetector::detect(output).expect("Should detect rustc error");
        assert_eq!(crash.source_type, CrashSourceType::Rustc);
        assert_eq!(crash.file_path, PathBuf::from("src/cockpit.rs"));
        assert_eq!(crash.line_number, 142);
        assert_eq!(crash.column_number, Some(25));
        assert_eq!(crash.error_type, "E0308");
        assert_eq!(crash.error_message, "mismatched types");

        let banner = crash.banner_text();
        assert!(banner.contains("⚠️ CRASH INTERCEPTOR: src/cockpit.rs:142 (E0308: mismatched types) [🚑 1-Click Heal]"));
    }

    #[test]
    fn test_rust_panic_interception() {
        let output = r#"
running 1 test
test test_state ... FAILED

failures:
---- test_state stdout ----
thread 'main' panicked at crates/hgb-core/src/state.rs:88:5:
assertion `left == right` failed
  left: 10
 right: 20
"#;
        let crash = CrashDetector::detect(output).expect("Should detect panic");
        assert_eq!(crash.source_type, CrashSourceType::UnhandledPanic);
        assert_eq!(crash.file_path, PathBuf::from("crates/hgb-core/src/state.rs"));
        assert_eq!(crash.line_number, 88);
        assert_eq!(crash.column_number, Some(5));
        assert_eq!(crash.error_type, "Panic");
        assert!(crash.error_message.contains("assertion `left == right` failed"));
    }

    #[test]
    fn test_python_traceback_interception() {
        let output = r#"
Traceback (most recent call last):
  File "scripts/devserver.py", line 55, in run_server
    raise ConnectionRefusedError("Redis cluster unreachable")
ConnectionRefusedError: Redis cluster unreachable
"#;
        let crash = CrashDetector::detect(output).expect("Should detect python traceback");
        assert_eq!(crash.source_type, CrashSourceType::PythonTraceback);
        assert_eq!(crash.file_path, PathBuf::from("scripts/devserver.py"));
        assert_eq!(crash.line_number, 55);
        assert_eq!(crash.error_type, "ConnectionRefusedError");
        assert_eq!(crash.error_message, "Redis cluster unreachable");
    }

    #[test]
    fn test_node_crash_interception() {
        let output = r#"
TypeError: Cannot read properties of undefined (reading 'map')
    at renderDashboard (/frontend/src/App.tsx:32:14)
    at invokeHook (/frontend/node_modules/react-dom/index.js:100:1)
"#;
        let crash = CrashDetector::detect(output).expect("Should detect node crash");
        assert_eq!(crash.source_type, CrashSourceType::NodeCrash);
        assert_eq!(crash.file_path, PathBuf::from("/frontend/src/App.tsx"));
        assert_eq!(crash.line_number, 32);
        assert_eq!(crash.column_number, Some(14));
        assert_eq!(crash.error_type, "TypeError");
        assert!(crash.error_message.contains("Cannot read properties of undefined"));
    }
}
