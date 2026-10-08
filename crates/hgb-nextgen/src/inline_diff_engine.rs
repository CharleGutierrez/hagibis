//! # High-Performance Visual Inline Diff Engine & Dual-Buffer Virtual Overlay
//!
//! Re-engineers Cursor & Cline's visual inline editing and diffing advantage:
//! - Sub-microsecond Myers SES & character-level intra-line diffing via Zig SIMD kernels.
//! - Dual-buffer virtual overlay for speculative model streaming with $O(1)$ Tab commit.
//! - TrueColor ANSI visual diff formatting (intra-line highlights, strike-throughs, side-by-side).
//! - Zero DOM/Electron overhead, operating directly on memory slices.

use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Operation type for a diff element
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffOpKind {
    Equal,
    Insertion,
    Deletion,
}

/// A contiguous span within a line highlighting fine-grained character/token changes
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineDiffSpan {
    pub kind: DiffOpKind,
    pub text: String,
}

/// A rendered line in an inline diff with character-level annotations
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineDiffLine {
    pub kind: DiffOpKind,
    pub old_line_num: Option<usize>,
    pub new_line_num: Option<usize>,
    pub content: String,
    pub spans: Vec<InlineDiffSpan>,
}

/// Metric summary of an inline diff calculation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InlineDiffMetrics {
    pub original_lines: usize,
    pub modified_lines: usize,
    pub added_lines: usize,
    pub deleted_lines: usize,
    pub similarity_ratio: f32,
    pub edit_distance: usize,
    pub compute_time_us: u64,
}

/// Dual-Buffer Virtual Overlay for speculative code streaming with O(1) Tab acceptance
#[derive(Debug, Clone)]
pub struct DualBufferOverlay {
    pub file_path: String,
    pub original_content: String,
    pub speculative_content: String,
    pub is_streaming: bool,
    pub accepted: Option<bool>,
}

impl DualBufferOverlay {
    pub fn new(file_path: impl Into<String>, original_content: impl Into<String>) -> Self {
        let orig = original_content.into();
        Self {
            file_path: file_path.into(),
            speculative_content: orig.clone(),
            original_content: orig,
            is_streaming: false,
            accepted: None,
        }
    }

    /// Stream speculative tokens into the secondary buffer without dirtying the original
    pub fn push_speculative_chunk(&mut self, chunk: &str) {
        if !self.is_streaming {
            self.speculative_content.clear();
            self.is_streaming = true;
        }
        self.speculative_content.push_str(chunk);
    }

    /// Replace speculative buffer content directly
    pub fn set_speculative_content(&mut self, content: impl Into<String>) {
        self.speculative_content = content.into();
        self.is_streaming = false;
    }

    /// O(1) commit upon user pressing Tab (pointer swap)
    pub fn commit_tab_acceptance(&mut self) -> String {
        self.accepted = Some(true);
        self.is_streaming = false;
        std::mem::swap(&mut self.original_content, &mut self.speculative_content);
        self.original_content.clone()
    }

    /// Discard speculative stream upon user pressing Esc
    pub fn discard_rejection(&mut self) {
        self.accepted = Some(false);
        self.is_streaming = false;
        self.speculative_content = self.original_content.clone();
    }

    /// Compute live inline diff representation between the dual buffers
    pub fn compute_diff(&self) -> (Vec<InlineDiffLine>, InlineDiffMetrics) {
        InlineDiffEngine::compute_inline_diff(&self.original_content, &self.speculative_content)
    }

    /// Render live TrueColor visual diff to terminal string
    pub fn render_ansi_diff(&self) -> String {
        InlineDiffEngine::render_ansi_inline_diff(&self.original_content, &self.speculative_content)
    }
}

/// Core Visual Inline Diffing Engine
pub struct InlineDiffEngine;

impl InlineDiffEngine {
    /// Compute high-precision inline diff with line-level and intra-line word annotations
    pub fn compute_inline_diff(original: &str, modified: &str) -> (Vec<InlineDiffLine>, InlineDiffMetrics) {
        let t0 = std::time::Instant::now();

        let orig_lines: Vec<&str> = original.lines().collect();
        let mod_lines: Vec<&str> = modified.lines().collect();

        let hashes_a: Vec<u64> = orig_lines.iter().map(|l| hash_str(l)).collect();
        let hashes_b: Vec<u64> = mod_lines.iter().map(|l| hash_str(l)).collect();

        // 1. Calculate similarity & distance via Zig Myers kernels
        let similarity = hgb_core::zig_accelerate::myers_lcs_similarity(&hashes_a, &hashes_b);
        let edit_distance = hgb_core::zig_accelerate::myers_diff_distance(&hashes_a, &hashes_b);

        // 2. Compute line-level LCS matching table via native Zig Myers SES kernel
        let mut diff_lines = Vec::new();
        let ses = hgb_core::zig_accelerate::inline_diff_ses(&hashes_a, &hashes_b);
        let lcs_pairs: Vec<(usize, usize)> = ses
            .into_iter()
            .filter(|item| item.op == hgb_core::zig_accelerate::InlineEditOp::Equal)
            .map(|item| (item.a_idx, item.b_idx))
            .collect();

        let mut a_idx = 0;
        let mut b_idx = 0;
        let mut added_count = 0;
        let mut deleted_count = 0;

        for (match_a, match_b) in lcs_pairs {
            // Lines deleted from A before match
            let del_start = a_idx;
            let del_end = match_a;

            // Lines inserted from B before match
            let ins_start = b_idx;
            let ins_end = match_b;

            // Check if there are modified lines (pairing deletions with additions)
            let del_len = del_end - del_start;
            let ins_len = ins_end - ins_start;

            if del_len > 0 && ins_len > 0 && del_len == ins_len {
                // Modified line replacement: compute fine-grained character spans
                for k in 0..del_len {
                    let old_l = orig_lines[del_start + k];
                    let new_l = mod_lines[ins_start + k];
                    let (del_spans, ins_spans) = compute_intra_line_spans(old_l, new_l);

                    diff_lines.push(InlineDiffLine {
                        kind: DiffOpKind::Deletion,
                        old_line_num: Some(del_start + k + 1),
                        new_line_num: None,
                        content: old_l.to_string(),
                        spans: del_spans,
                    });
                    deleted_count += 1;

                    diff_lines.push(InlineDiffLine {
                        kind: DiffOpKind::Insertion,
                        old_line_num: None,
                        new_line_num: Some(ins_start + k + 1),
                        content: new_l.to_string(),
                        spans: ins_spans,
                    });
                    added_count += 1;
                }
            } else {
                for i in del_start..del_end {
                    let old_l = orig_lines[i];
                    diff_lines.push(InlineDiffLine {
                        kind: DiffOpKind::Deletion,
                        old_line_num: Some(i + 1),
                        new_line_num: None,
                        content: old_l.to_string(),
                        spans: vec![InlineDiffSpan {
                            kind: DiffOpKind::Deletion,
                            text: old_l.to_string(),
                        }],
                    });
                    deleted_count += 1;
                }

                for j in ins_start..ins_end {
                    let new_l = mod_lines[j];
                    diff_lines.push(InlineDiffLine {
                        kind: DiffOpKind::Insertion,
                        old_line_num: None,
                        new_line_num: Some(j + 1),
                        content: new_l.to_string(),
                        spans: vec![InlineDiffSpan {
                            kind: DiffOpKind::Insertion,
                            text: new_l.to_string(),
                        }],
                    });
                    added_count += 1;
                }
            }

            // Matching equal line
            let eq_l = orig_lines[match_a];
            diff_lines.push(InlineDiffLine {
                kind: DiffOpKind::Equal,
                old_line_num: Some(match_a + 1),
                new_line_num: Some(match_b + 1),
                content: eq_l.to_string(),
                spans: vec![InlineDiffSpan {
                    kind: DiffOpKind::Equal,
                    text: eq_l.to_string(),
                }],
            });

            a_idx = match_a + 1;
            b_idx = match_b + 1;
        }

        // Remaining tail deletions
        while a_idx < orig_lines.len() {
            let old_l = orig_lines[a_idx];
            diff_lines.push(InlineDiffLine {
                kind: DiffOpKind::Deletion,
                old_line_num: Some(a_idx + 1),
                new_line_num: None,
                content: old_l.to_string(),
                spans: vec![InlineDiffSpan {
                    kind: DiffOpKind::Deletion,
                    text: old_l.to_string(),
                }],
            });
            deleted_count += 1;
            a_idx += 1;
        }

        // Remaining tail insertions
        while b_idx < mod_lines.len() {
            let new_l = mod_lines[b_idx];
            diff_lines.push(InlineDiffLine {
                kind: DiffOpKind::Insertion,
                old_line_num: None,
                new_line_num: Some(b_idx + 1),
                content: new_l.to_string(),
                spans: vec![InlineDiffSpan {
                    kind: DiffOpKind::Insertion,
                    text: new_l.to_string(),
                }],
            });
            added_count += 1;
            b_idx += 1;
        }

        let elapsed_us = t0.elapsed().as_micros() as u64;

        let metrics = InlineDiffMetrics {
            original_lines: orig_lines.len(),
            modified_lines: mod_lines.len(),
            added_lines: added_count,
            deleted_lines: deleted_count,
            similarity_ratio: similarity,
            edit_distance,
            compute_time_us: elapsed_us,
        };

        (diff_lines, metrics)
    }

    /// Render diff to rich TrueColor ANSI terminal output with intra-line highlights
    pub fn render_ansi_inline_diff(original: &str, modified: &str) -> String {
        let (lines, metrics) = Self::compute_inline_diff(original, modified);
        let mut out = String::new();

        out.push_str(&format!(
            "\x1b[1;38;2;120;180;255m┌── HAGIBIS INLINE DIFF COCKPIT ──────────────────────────────────────────────┐\x1b[0m\n\
             \x1b[38;2;160;160;160m│ Metric: +{} lines / -{} lines | Similarity: {:.1}% | Diff Time: {} µs\x1b[0m\n\
             \x1b[1;38;2;120;180;255m├───┬───┬────────────────────────────────────────────────────────────────────────┤\x1b[0m\n",
            metrics.added_lines,
            metrics.deleted_lines,
            metrics.similarity_ratio * 100.0,
            metrics.compute_time_us
        ));

        for line in lines {
            match line.kind {
                DiffOpKind::Equal => {
                    let old_no = line.old_line_num.map(|n| format!("{:>3}", n)).unwrap_or_else(|| "   ".into());
                    let new_no = line.new_line_num.map(|n| format!("{:>3}", n)).unwrap_or_else(|| "   ".into());
                    out.push_str(&format!(
                        "\x1b[38;2;100;100;100m│{}│{}│ \x1b[38;2;180;180;180m{}\x1b[0m\n",
                        old_no, new_no, line.content
                    ));
                }
                DiffOpKind::Deletion => {
                    let old_no = line.old_line_num.map(|n| format!("{:>3}", n)).unwrap_or_else(|| "   ".into());
                    out.push_str(&format!(
                        "\x1b[48;2;45;15;15m\x1b[38;2;240;80;80m│{}│   │ - \x1b[0m\x1b[48;2;45;15;15m",
                        old_no
                    ));
                    for span in &line.spans {
                        if span.kind == DiffOpKind::Deletion {
                            // Intra-line deletion: bright red + strikethrough + darker bg
                            out.push_str(&format!(
                                "\x1b[48;2;80;20;20m\x1b[1;38;2;255;120;120m\x1b[9m{}\x1b[0m\x1b[48;2;45;15;15m",
                                span.text
                            ));
                        } else {
                            out.push_str(&format!("\x1b[38;2;220;90;90m{}\x1b[0m", span.text));
                        }
                    }
                    out.push_str("\x1b[0m\n");
                }
                DiffOpKind::Insertion => {
                    let new_no = line.new_line_num.map(|n| format!("{:>3}", n)).unwrap_or_else(|| "   ".into());
                    out.push_str(&format!(
                        "\x1b[48;2;15;45;20m\x1b[38;2;80;240;120m│   │{}│ + \x1b[0m\x1b[48;2;15;45;20m",
                        new_no
                    ));
                    for span in &line.spans {
                        if span.kind == DiffOpKind::Insertion {
                            // Intra-line insertion: bright green bold + highlight bg
                            out.push_str(&format!(
                                "\x1b[48;2;25;80;35m\x1b[1;38;2;140;255;160m{}\x1b[0m\x1b[48;2;15;45;20m",
                                span.text
                            ));
                        } else {
                            out.push_str(&format!("\x1b[38;2;90;220;120m{}\x1b[0m", span.text));
                        }
                    }
                    out.push_str("\x1b[0m\n");
                }
            }
        }

        out.push_str("\x1b[1;38;2;120;180;255m└───┴───┴────────────────────────────────────────────────────────────────────────┘\x1b[0m\n");
        out
    }

    /// Render side-by-side split visual diff
    pub fn render_side_by_side(original: &str, modified: &str, col_width: usize) -> String {
        let (lines, _) = Self::compute_inline_diff(original, modified);
        let mut out = String::new();
        let col = col_width.max(30);

        out.push_str(&format!(
            "\x1b[1;38;2;120;180;255m┌─ ORIGINAL {:width$} ┬─ MODIFIED {:width$} ┐\x1b[0m\n",
            "", "", width = col.saturating_sub(12)
        ));

        let mut i = 0;
        while i < lines.len() {
            let cur = &lines[i];
            if cur.kind == DiffOpKind::Equal {
                let left = truncate_pad(&cur.content, col);
                let right = truncate_pad(&cur.content, col);
                out.push_str(&format!("│ \x1b[38;2;160;160;160m{}\x1b[0m │ \x1b[38;2;160;160;160m{}\x1b[0m │\n", left, right));
                i += 1;
            } else if cur.kind == DiffOpKind::Deletion {
                let left = truncate_pad(&cur.content, col);
                if i + 1 < lines.len() && lines[i + 1].kind == DiffOpKind::Insertion {
                    let right = truncate_pad(&lines[i + 1].content, col);
                    out.push_str(&format!(
                        "│ \x1b[38;2;255;90;90m{}\x1b[0m │ \x1b[38;2;90;255;120m{}\x1b[0m │\n",
                        left, right
                    ));
                    i += 2;
                } else {
                    let right = truncate_pad("", col);
                    out.push_str(&format!(
                        "│ \x1b[38;2;255;90;90m{}\x1b[0m │ {} │\n",
                        left, right
                    ));
                    i += 1;
                }
            } else {
                let left = truncate_pad("", col);
                let right = truncate_pad(&cur.content, col);
                out.push_str(&format!(
                    "│ {} │ \x1b[38;2;90;255;120m{}\x1b[0m │\n",
                    left, right
                ));
                i += 1;
            }
        }

        out.push_str(&format!(
            "\x1b[1;38;2;120;180;255m└{:─<width$}─┴─{:─<width$}─┘\x1b[0m\n",
            "", "", width = col + 1
        ));
        out
    }
}

fn truncate_pad(s: &str, width: usize) -> String {
    if s.len() > width {
        format!("{}…", &s[..width.saturating_sub(1)])
    } else {
        format!("{:width$}", s, width = width)
    }
}

fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}



/// Compute character-level intra-line difference spans via native Zig Myers character kernel
fn compute_intra_line_spans(old_line: &str, new_line: &str) -> (Vec<InlineDiffSpan>, Vec<InlineDiffSpan>) {
    let items = hgb_core::zig_accelerate::inline_diff_chars(old_line, new_line);
    let mut del_spans = Vec::new();
    let mut ins_spans = Vec::new();

    let old_chars: Vec<char> = old_line.chars().collect();
    let new_chars: Vec<char> = new_line.chars().collect();

    for item in items {
        match item.op {
            hgb_core::zig_accelerate::InlineEditOp::Equal => {
                if let Some(&ch) = old_chars.get(item.a_idx) {
                    append_char_span(&mut del_spans, DiffOpKind::Equal, ch);
                    append_char_span(&mut ins_spans, DiffOpKind::Equal, ch);
                }
            }
            hgb_core::zig_accelerate::InlineEditOp::Delete => {
                if let Some(&ch) = old_chars.get(item.a_idx) {
                    append_char_span(&mut del_spans, DiffOpKind::Deletion, ch);
                }
            }
            hgb_core::zig_accelerate::InlineEditOp::Insert => {
                if let Some(&ch) = new_chars.get(item.b_idx) {
                    append_char_span(&mut ins_spans, DiffOpKind::Insertion, ch);
                }
            }
        }
    }

    if del_spans.is_empty() && !old_line.is_empty() {
        del_spans.push(InlineDiffSpan {
            kind: DiffOpKind::Deletion,
            text: old_line.to_string(),
        });
    }
    if ins_spans.is_empty() && !new_line.is_empty() {
        ins_spans.push(InlineDiffSpan {
            kind: DiffOpKind::Insertion,
            text: new_line.to_string(),
        });
    }

    (del_spans, ins_spans)
}

fn append_char_span(spans: &mut Vec<InlineDiffSpan>, kind: DiffOpKind, ch: char) {
    if let Some(last) = spans.last_mut() {
        if last.kind == kind {
            last.text.push(ch);
            return;
        }
    }
    spans.push(InlineDiffSpan {
        kind,
        text: ch.to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inline_diff_computation_and_metrics() {
        let orig = "fn calculate(x: i32) -> i32 {\n    x * 2\n}\n";
        let modified = "fn calculate(x: i32) -> i32 {\n    x * 4\n}\n";

        let (lines, metrics) = InlineDiffEngine::compute_inline_diff(orig, modified);
        assert!(metrics.similarity_ratio > 0.6);
        assert_eq!(metrics.added_lines, 1);
        assert_eq!(metrics.deleted_lines, 1);

        // Verify intra-line spans for modified line
        let del_line = lines.iter().find(|l| l.kind == DiffOpKind::Deletion).unwrap();
        assert!(del_line.spans.iter().any(|s| s.kind == DiffOpKind::Deletion && s.text == "2"));

        let ins_line = lines.iter().find(|l| l.kind == DiffOpKind::Insertion).unwrap();
        assert!(ins_line.spans.iter().any(|s| s.kind == DiffOpKind::Insertion && s.text == "4"));
    }

    #[test]
    fn test_dual_buffer_overlay_lifecycle() {
        let mut overlay = DualBufferOverlay::new("main.rs", "let a = 1;");
        overlay.push_speculative_chunk("let a = 2;");

        assert_eq!(overlay.original_content, "let a = 1;");
        assert_eq!(overlay.speculative_content, "let a = 2;");

        // Commit via Tab
        let committed = overlay.commit_tab_acceptance();
        assert_eq!(committed, "let a = 2;");
        assert_eq!(overlay.original_content, "let a = 2;");
    }

    #[test]
    fn test_ansi_and_side_by_side_rendering() {
        let a = "hello world\nline 2\n";
        let b = "hello brave new world\nline 2\nline 3\n";

        let ansi = InlineDiffEngine::render_ansi_inline_diff(a, b);
        assert!(ansi.contains("HAGIBIS INLINE DIFF COCKPIT"));
        assert!(ansi.contains("+2 lines"));

        let sbs = InlineDiffEngine::render_side_by_side(a, b, 40);
        assert!(sbs.contains("ORIGINAL"));
        assert!(sbs.contains("MODIFIED"));
    }
}
