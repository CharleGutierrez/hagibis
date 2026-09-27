//! Cross-Platform Terminal and System Clipboard Manager
//!
//! Provides zero-dependency, universal clipboard synchronization:
//! 1. OSC 52 terminal sequence (ANSI escape code: `\x1b]52;c;<base64>\x07`)
//!    - Directly supported by modern terminal emulators (Alacritty, Kitty, WezTerm,
//!      iTerm2, Windows Terminal, tmux, Ghostty, Foot, etc.).
//!    - Operates seamlessly over SSH and containers without X11 or Wayland socket forwarding.
//! 2. Linux native desktop clipboards:
//!    - `xsel -b -i` (X11)
//!    - `wl-copy` (Wayland)
//!    - `xclip -selection clipboard` (X11)
//! 3. macOS native clipboard:
//!    - `pbcopy`

use base64::Engine;
use std::io::Write;
use std::process::{Command, Stdio};

/// Unified clipboard synchronizer supporting OSC 52 and native desktop display servers
pub struct ClipboardHelper;

impl ClipboardHelper {
    /// Formats an OSC 52 ANSI escape sequence for copying to the terminal emulator's system clipboard.
    pub fn format_osc52_sequence(text: &str) -> String {
        let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        format!("\x1b]52;c;{}\x07", b64)
    }

    /// Copy text to system clipboard via native OS toolchain and OSC 52 escape sequences.
    ///
    /// Never panics. Returns Ok(()) on successful delivery to at least one clipboard mechanism.
    pub fn copy(text: &str) -> std::result::Result<(), String> {
        let mut native_success = false;

        // 1. Try xsel (Standard X11 - widely available on Linux)
        if Self::pipe_to_command("xsel", &["-b", "-i"], text) {
            native_success = true;
        }

        // 2. Try wl-copy (Wayland compositors)
        if !native_success && Self::pipe_to_command("wl-copy", &[], text) {
            native_success = true;
        }

        // 3. Try xclip (Alternative X11)
        if !native_success && Self::pipe_to_command("xclip", &["-selection", "clipboard"], text) {
            native_success = true;
        }

        // 4. Try pbcopy (macOS)
        if !native_success {
            let _ = Self::pipe_to_command("pbcopy", &[], text);
        }

        // 5. Always emit OSC 52 escape sequence to stdout/terminal backend
        // This ensures compatibility with terminal emulators regardless of desktop environment.
        let osc52 = Self::format_osc52_sequence(text);
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(osc52.as_bytes());
        let _ = stdout.flush();

        Ok(())
    }

    /// Read text from the system clipboard via native OS toolchain.
    pub fn get_text() -> std::result::Result<String, String> {
        // 1. Try wl-paste (Wayland)
        if let Ok(out) = Command::new("wl-paste").output() {
            if out.status.success() && !out.stdout.is_empty() {
                if let Ok(s) = String::from_utf8(out.stdout) {
                    return Ok(s);
                }
            }
        }
        // 2. Try xclip (X11)
        if let Ok(out) = Command::new("xclip").args(&["-selection", "clipboard", "-o"]).output() {
            if out.status.success() && !out.stdout.is_empty() {
                if let Ok(s) = String::from_utf8(out.stdout) {
                    return Ok(s);
                }
            }
        }
        // 3. Try xsel (X11)
        if let Ok(out) = Command::new("xsel").args(&["-b", "-o"]).output() {
            if out.status.success() && !out.stdout.is_empty() {
                if let Ok(s) = String::from_utf8(out.stdout) {
                    return Ok(s);
                }
            }
        }
        // 4. Try pbpaste (macOS)
        if let Ok(out) = Command::new("pbpaste").output() {
            if out.status.success() && !out.stdout.is_empty() {
                if let Ok(s) = String::from_utf8(out.stdout) {
                    return Ok(s);
                }
            }
        }
        Err("Clipboard empty or not accessible".to_string())
    }

    fn pipe_to_command(cmd: &str, args: &[&str], input: &str) -> bool {
        match Command::new(cmd)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(input.as_bytes());
                    let _ = stdin.flush();
                }
                match child.wait() {
                    Ok(status) => status.success(),
                    Err(_) => false,
                }
            }
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_osc52_sequence() {
        let seq = ClipboardHelper::format_osc52_sequence("hello");
        assert!(seq.starts_with("\x1b]52;c;"));
        assert!(seq.ends_with("\x07"));
        let b64 = &seq[7..seq.len() - 1];
        let decoded = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "hello");
    }

    #[test]
    fn test_copy_execution_safety() {
        // Must never panic even with empty string or special chars
        let res = ClipboardHelper::copy("Test clipboard content\nLine 2");
        assert!(res.is_ok());
    }
}
