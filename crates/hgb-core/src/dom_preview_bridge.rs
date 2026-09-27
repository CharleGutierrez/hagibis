//! # DomPreviewBridge - Visual Click-to-Code & DOM Inspector
//!
//! Elevates Bolt.new & Lovable's visual click-to-code telemetry. Injects AST
//! source coordinates into frontend templates, tracks element bounding boxes,
//! and maps browser coordinate clicks directly to source code lines.

use regex::Regex;
use serde::{Deserialize, Serialize};

/// 2D Bounding Box of rendered DOM element
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct BoundingBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl BoundingBox {
    pub fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x && px <= (self.x + self.width) && py >= self.y && py <= (self.y + self.height)
    }

    pub fn area(&self) -> f64 {
        self.width * self.height
    }
}

/// Discovered DOM element linked to source coordinates
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DomElement {
    pub tag: String,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub text_preview: Option<String>,
    pub source_file: String,
    pub source_line: usize,
    pub bounding_box: Option<BoundingBox>,
}

pub struct DomPreviewBridge;

impl DomPreviewBridge {
    /// Injects `data-hgb-source="path:line"` attributes into HTML/JSX elements
    pub fn inject_source_telemetry(content: &str, file_path: &str) -> String {
        let tag_open_re = Regex::new(r"<([a-zA-Z0-9_\-]+)(\s+[^>]*?)?(/?\s*)>").unwrap();
        let mut out = String::new();

        for (idx, line) in content.lines().enumerate() {
            let line_no = idx + 1;
            let replaced = tag_open_re.replace_all(line, |caps: &regex::Captures| {
                let tag = &caps[1];
                // Skip doctype or closing tags
                if tag.starts_with("!") || tag.starts_with("/") {
                    return caps[0].to_string();
                }
                let attrs = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                let closing = &caps[3];

                // Don't double inject
                if attrs.contains("data-hgb-source") {
                    return caps[0].to_string();
                }

                format!(
                    "<{} data-hgb-source=\"{}:{}\"{}{}>",
                    tag, file_path, line_no, attrs, closing
                )
            });
            out.push_str(&replaced);
            out.push('\n');
        }

        out
    }

    /// Parse HTML/JSX source into structured DomElements
    pub fn parse_elements(content: &str, file_path: &str) -> Vec<DomElement> {
        let tag_re = Regex::new(r"<([a-zA-Z0-9_\-]+)([^>]*?)>").unwrap();
        let id_re = Regex::new(r#"id=["']([^"']+)["']"#).unwrap();
        let class_re = Regex::new(r#"(?:class|className)=["']([^"']+)["']"#).unwrap();

        let mut elements = Vec::new();

        for (idx, line) in content.lines().enumerate() {
            let line_no = idx + 1;
            for caps in tag_re.captures_iter(line) {
                let tag = caps[1].to_string();
                if tag.starts_with('/') || tag.starts_with('!') {
                    continue;
                }
                let attrs = &caps[2];

                let id = id_re.captures(attrs).map(|c| c[1].to_string());
                let classes = if let Some(c) = class_re.captures(attrs) {
                    c[1].split_whitespace().map(|s| s.to_string()).collect()
                } else {
                    Vec::new()
                };

                // Approximate text content if present in same line
                let text_preview = if let Some(close_idx) = line.find('>') {
                    let rem = &line[close_idx + 1..];
                    if let Some(open_idx) = rem.find('<') {
                        let text = rem[..open_idx].trim();
                        if !text.is_empty() {
                            Some(text.to_string())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                elements.push(DomElement {
                    tag,
                    id,
                    classes,
                    text_preview,
                    source_file: file_path.to_string(),
                    source_line: line_no,
                    bounding_box: None,
                });
            }
        }

        elements
    }

    /// Resolve a screen coordinate (x, y) to the deepest nested DOM element
    pub fn resolve_coordinate(elements: &[DomElement], x: f64, y: f64) -> Option<&DomElement> {
        let mut matching: Vec<&DomElement> = elements
            .iter()
            .filter(|e| {
                if let Some(bbox) = e.bounding_box {
                    bbox.contains(x, y)
                } else {
                    false
                }
            })
            .collect();

        // Sort by area ascending (smallest area is the most specific/deepest element)
        matching.sort_by(|a, b| {
            let area_a = a.bounding_box.map(|b| b.area()).unwrap_or(f64::MAX);
            let area_b = b.bounding_box.map(|b| b.area()).unwrap_or(f64::MAX);
            area_a.partial_cmp(&area_b).unwrap_or(std::cmp::Ordering::Equal)
        });

        matching.first().copied()
    }

    /// Query elements by CSS selector (.class, #id, tag)
    pub fn query_selector<'a>(elements: &'a [DomElement], selector: &str) -> Vec<&'a DomElement> {
        let sel = selector.trim();
        if let Some(id_target) = sel.strip_prefix('#') {
            elements
                .iter()
                .filter(|e| e.id.as_deref() == Some(id_target))
                .collect()
        } else if let Some(class_target) = sel.strip_prefix('.') {
            elements
                .iter()
                .filter(|e| e.classes.iter().any(|c| c == class_target))
                .collect()
        } else {
            elements.iter().filter(|e| e.tag.eq_ignore_ascii_case(sel)).collect()
        }
    }

    /// Format a visual hierarchy layout map for the AI model
    pub fn format_visual_hierarchy(elements: &[DomElement]) -> String {
        let mut out = String::new();
        out.push_str("=== VISUAL DOM HIERARCHY MAP ===\n");
        for el in elements {
            let id_str = el.id.as_ref().map(|i| format!("#{}", i)).unwrap_or_default();
            let cls_str = if !el.classes.is_empty() {
                format!(".{}", el.classes.join("."))
            } else {
                String::new()
            };
            let text_str = el.text_preview.as_ref().map(|t| format!(" \"{}\"", t)).unwrap_or_default();
            let bbox_str = el.bounding_box.map(|b| format!(" [{}x{} @ ({},{})]", b.width, b.height, b.x, b.y)).unwrap_or_default();

            out.push_str(&format!(
                "<{}{}{}>{} -> {}:{}{}\n",
                el.tag, id_str, cls_str, text_str, el.source_file, el.source_line, bbox_str
            ));
        }
        out
    }
}
