//! # CdpTweakMirror - Bidirectional DevTools Click-to-Source Sync
//!
//! Elevates Bolt.new & Devin live visual inspector synchronization.
//! Bridges browser DOM element tweaks directly back into source JSX, TSX, Vue,
//! or HTML templates with surgical AST/attribute-aware element matching and zero page reloads.

use crate::error::{HgbError, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Event generated when a DOM element is adjusted in browser DevTools or Click-to-Code UI
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomTweakEvent {
    pub selector: String,
    pub property_or_attr: String,
    pub old_value: String,
    pub new_value: String,
    pub component_hint: Option<String>,
    pub file_hint: Option<String>,
}

/// Verification report after applying a visual DOM tweak back to source files
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TweakSyncReport {
    pub target_file: String,
    pub matched_line: usize,
    pub old_line: String,
    pub new_line: String,
    pub diff_applied: String,
    pub file_written: bool,
    pub blake3_hash: String,
    pub success: bool,
    pub message: String,
}

/// Inspected DOM element report with AST coordinates and classes
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InspectedDomElement {
    pub tag_name: String,
    pub id: Option<String>,
    pub class_name: Option<String>,
    pub classes: Vec<String>,
    pub attributes: std::collections::HashMap<String, String>,
    pub start_line: usize,
    pub end_line: usize,
    pub component_scope: Option<String>,
    pub is_self_closing: bool,
    pub indentation: String,
    pub raw_snippet: String,
}

/// Specification for a visual DOM tweak / patch operation
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VisualPatchSpec {
    pub target_file: Option<String>,
    pub selector: String,
    pub component_hint: Option<String>,
    pub added_classes: Vec<String>,
    pub removed_classes: Vec<String>,
    pub updated_attributes: std::collections::HashMap<String, String>,
}

/// Parsed JSX/TSX attribute representation
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsxAttribute {
    pub name: String,
    pub raw_value: String,
    pub inner_value: String,
    pub start_byte: usize,
    pub end_byte: usize,
    pub value_start_byte: usize,
    pub value_end_byte: usize,
}

/// AST-Aware JSX/TSX element representation
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsxElement {
    pub tag_name: String,
    pub attributes: Vec<JsxAttribute>,
    pub component_scope: Option<String>,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_line: usize,
    pub end_line: usize,
    pub is_self_closing: bool,
}

/// Parsed CSS selector structure for AST element scoring
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedSelector {
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub component_scope: Option<String>,
}

impl ParsedSelector {
    pub fn parse(selector: &str, component_hint: Option<&str>) -> Self {
        let mut parsed = Self::default();
        parsed.component_scope = component_hint.map(|s| s.to_string());

        let sel = selector.trim();
        if sel.is_empty() {
            return parsed;
        }

        // Check for combinators or ancestor components like "Header > button.checkout" or "Header button"
        let parts: Vec<&str> = if sel.contains('>') {
            sel.split('>').map(|s| s.trim()).collect()
        } else {
            sel.split_whitespace().collect()
        };

        if parts.len() > 1 && parsed.component_scope.is_none() {
            let ancestor = parts[0].trim_start_matches('.').trim_start_matches('#');
            if !ancestor.is_empty() && ancestor.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                parsed.component_scope = Some(ancestor.to_string());
            }
        }

        let target_part = parts.last().copied().unwrap_or(sel);

        // Parse tag, id, and classes from target part (e.g. "button#submit-btn.flex.items-center")
        let mut current_token = String::new();
        let mut state = 0; // 0: tag, 1: id, 2: class

        for ch in target_part.chars() {
            match ch {
                '#' => {
                    if state == 0 && !current_token.is_empty() {
                        parsed.tag = Some(current_token.clone());
                    } else if state == 2 && !current_token.is_empty() {
                        parsed.classes.push(current_token.clone());
                    }
                    current_token.clear();
                    state = 1;
                }
                '.' => {
                    if state == 0 && !current_token.is_empty() {
                        parsed.tag = Some(current_token.clone());
                    } else if state == 1 && !current_token.is_empty() {
                        parsed.id = Some(current_token.clone());
                    } else if state == 2 && !current_token.is_empty() {
                        parsed.classes.push(current_token.clone());
                    }
                    current_token.clear();
                    state = 2;
                }
                _ => {
                    current_token.push(ch);
                }
            }
        }

        if !current_token.is_empty() {
            match state {
                0 => parsed.tag = Some(current_token),
                1 => parsed.id = Some(current_token),
                2 => parsed.classes.push(current_token),
                _ => {}
            }
        }

        parsed
    }
}

pub struct CdpTweakMirror {
    pub workspace_root: PathBuf,
}

impl CdpTweakMirror {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
        }
    }

    /// Accurately inspects a DOM/JSX element by selector in a source file or string buffer
    pub fn inspect_dom_element(&self, file_path_or_content: &str, selector: &str) -> Option<InspectedDomElement> {
        let content = if file_path_or_content.contains('<') || file_path_or_content.contains('\n') {
            file_path_or_content.to_string()
        } else {
            let path = self.workspace_root.join(file_path_or_content);
            if path.exists() {
                fs::read_to_string(&path).unwrap_or_else(|_| file_path_or_content.to_string())
            } else {
                file_path_or_content.to_string()
            }
        };

        let elements = Self::parse_jsx_elements(&content);
        let parsed_selector = ParsedSelector::parse(selector, None);
        let mut best: Option<(&JsxElement, i32)> = None;

        for elem in &elements {
            let dummy_event = DomTweakEvent {
                selector: selector.to_string(),
                property_or_attr: "className".to_string(),
                old_value: String::new(),
                new_value: String::new(),
                component_hint: None,
                file_hint: None,
            };
            let score = Self::score_element(elem, &dummy_event, &parsed_selector);
            if score > 0 {
                if let Some((_, best_score)) = best {
                    if score > best_score {
                        best = Some((elem, score));
                    }
                } else {
                    best = Some((elem, score));
                }
            }
        }

        let (elem, _) = best?;
        let mut attributes = std::collections::HashMap::new();
        let mut class_name = None;
        let mut id = None;

        for attr in &elem.attributes {
            attributes.insert(attr.name.clone(), attr.inner_value.clone());
            if attr.name.eq_ignore_ascii_case("className") || attr.name.eq_ignore_ascii_case("class") {
                class_name = Some(attr.inner_value.clone());
            } else if attr.name.eq_ignore_ascii_case("id") {
                id = Some(attr.inner_value.clone());
            }
        }

        let classes: Vec<String> = class_name
            .as_deref()
            .map(|c| c.split_whitespace().map(|s| s.to_string()).collect())
            .unwrap_or_default();

        let raw_snippet = content[elem.start_byte..elem.end_byte.min(content.len())].to_string();

        let line_starts = Self::compute_line_starts(&content);
        let line_start_byte = line_starts.get(elem.start_line.saturating_sub(1)).copied().unwrap_or(0);
        let indentation = content[line_start_byte..elem.start_byte.min(content.len())]
            .chars()
            .take_while(|c| c.is_whitespace() && *c != '\n')
            .collect();

        Some(InspectedDomElement {
            tag_name: elem.tag_name.clone(),
            id,
            class_name,
            classes,
            attributes,
            start_line: elem.start_line,
            end_line: elem.end_line,
            component_scope: elem.component_scope.clone(),
            is_self_closing: elem.is_self_closing,
            indentation,
            raw_snippet,
        })
    }

    /// Surgically applies a visual patch (class additions/removals, attribute updates) to JSX/HTML
    /// while strictly preserving indentation and surrounding syntax.
    pub fn apply_visual_patch(
        &self,
        target_file_or_content: &str,
        selector: &str,
        added_classes: &[String],
        removed_classes: &[String],
        attribute_updates: &std::collections::HashMap<String, String>,
    ) -> Result<String> {
        let (content, is_file_on_disk, file_path) = if target_file_or_content.contains('<') || target_file_or_content.contains('\n') {
            (target_file_or_content.to_string(), false, None)
        } else {
            let path = self.workspace_root.join(target_file_or_content);
            if path.exists() {
                let text = fs::read_to_string(&path).map_err(HgbError::Io)?;
                (text, true, Some(path))
            } else {
                (target_file_or_content.to_string(), false, None)
            }
        };

        let elements = Self::parse_jsx_elements(&content);
        let parsed_selector = ParsedSelector::parse(selector, None);
        let mut best: Option<(&JsxElement, i32)> = None;

        for elem in &elements {
            let dummy_event = DomTweakEvent {
                selector: selector.to_string(),
                property_or_attr: "className".to_string(),
                old_value: String::new(),
                new_value: String::new(),
                component_hint: None,
                file_hint: None,
            };
            let score = Self::score_element(elem, &dummy_event, &parsed_selector);
            if score > 0 {
                if let Some((_, best_score)) = best {
                    if score > best_score {
                        best = Some((elem, score));
                    }
                } else {
                    best = Some((elem, score));
                }
            }
        }

        let elem = match best {
            Some((e, _)) => e,
            None => return Ok(content),
        };

        let mut updated = content.clone();

        // 1. Update/Add attributes
        for (attr_name, new_val) in attribute_updates {
            if let Some(existing_attr) = elem.attributes.iter().find(|a| a.name.eq_ignore_ascii_case(attr_name)) {
                let start = existing_attr.value_start_byte;
                let end = existing_attr.value_end_byte;
                if start < updated.len() && end <= updated.len() {
                    updated = format!("{}{}{}", &updated[..start], new_val, &updated[end..]);
                }
            } else {
                let insert_pos = elem.start_byte + 1 + elem.tag_name.len();
                if insert_pos <= updated.len() {
                    let insertion = format!(" {}=\"{}\"", attr_name, new_val);
                    updated = format!("{}{}{}", &updated[..insert_pos], insertion, &updated[insert_pos..]);
                }
            }
        }

        // 2. Add / Remove CSS classes
        if !added_classes.is_empty() || !removed_classes.is_empty() {
            let class_attr = elem.attributes.iter().find(|a| {
                a.name.eq_ignore_ascii_case("className") || a.name.eq_ignore_ascii_case("class")
            });

            if let Some(attr) = class_attr {
                let mut tokens: Vec<String> = attr
                    .inner_value
                    .split_whitespace()
                    .filter(|t| !removed_classes.iter().any(|r| r == *t))
                    .map(|s| s.to_string())
                    .collect();

                for added in added_classes {
                    if !tokens.contains(added) {
                        tokens.push(added.clone());
                    }
                }

                let new_class_value = tokens.join(" ");
                let target_attr_name = &attr.name;
                let needle = format!("{}=\"{}\"", target_attr_name, attr.inner_value);
                if let Some(pos) = updated.find(&needle) {
                    let rep = format!("{}=\"{}\"", target_attr_name, new_class_value);
                    updated = format!("{}{}{}", &updated[..pos], rep, &updated[pos + needle.len()..]);
                }
            } else if !added_classes.is_empty() {
                let insert_pos = elem.start_byte + 1 + elem.tag_name.len();
                let class_prop = if content.contains("className") { "className" } else { "class" };
                let insertion = format!(" {}=\"{}\"", class_prop, added_classes.join(" "));
                if insert_pos <= updated.len() {
                    updated = format!("{}{}{}", &updated[..insert_pos], insertion, &updated[insert_pos..]);
                }
            }
        }

        if is_file_on_disk {
            if let Some(ref p) = file_path {
                fs::write(p, &updated).map_err(HgbError::Io)?;
            }
        }

        Ok(updated)
    }

    /// Surgically synchronize a browser DOM tweak into the repository source file
    /// utilizing AST/attribute-aware JSX matching to avoid colliding on common classes like `flex items-center`.
    pub fn sync_tweak(&self, event: &DomTweakEvent, apply_to_disk: bool) -> Result<TweakSyncReport> {
        let target_file_path = self.find_candidate_file(event)?;
        let content = fs::read_to_string(&target_file_path).map_err(HgbError::Io)?;

        // 1. Try AST / tag-attribute aware JSX element synchronization first
        if let Some(report) = self.sync_jsx_ast_tweak(&target_file_path, &content, event, apply_to_disk)? {
            return Ok(report);
        }

        // 2. Fallback to contextual line replacement for plain HTML or non-JSX files
        self.sync_line_fallback(&target_file_path, &content, event, apply_to_disk)
    }

    /// AST / Tag-Attribute Aware JSX/TSX element synchronization
    fn sync_jsx_ast_tweak(
        &self,
        target_file_path: &Path,
        content: &str,
        event: &DomTweakEvent,
        apply_to_disk: bool,
    ) -> Result<Option<TweakSyncReport>> {
        let elements = Self::parse_jsx_elements(content);
        if elements.is_empty() {
            return Ok(None);
        }

        let parsed_selector = ParsedSelector::parse(&event.selector, event.component_hint.as_deref());

        // Score each element candidate
        let mut best_element: Option<(&JsxElement, i32)> = None;
        for elem in &elements {
            let score = Self::score_element(elem, event, &parsed_selector);
            if score > 0 {
                match best_element {
                    Some((_, best_score)) if score > best_score => {
                        best_element = Some((elem, score));
                    }
                    None => {
                        best_element = Some((elem, score));
                    }
                    _ => {}
                }
            }
        }

        let (matched_elem, _score) = match best_element {
            Some(pair) => pair,
            None => return Ok(None),
        };

        // Locate target attribute to modify
        let target_attr = matched_elem
            .attributes
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(&event.property_or_attr));

        let (replacement_range, new_text) = if let Some(attr) = target_attr {
            // Existing attribute found on the matched element
            if attr.inner_value.contains(&event.old_value) && !event.old_value.is_empty() {
                let replaced = attr.inner_value.replace(&event.old_value, &event.new_value);
                let start = attr.value_start_byte;
                let end = attr.value_end_byte;
                (start..end, replaced)
            } else if event.property_or_attr.eq_ignore_ascii_case("className") || event.property_or_attr.eq_ignore_ascii_case("class") {
                // Class token replacement
                let replaced = Self::replace_class_tokens(&attr.inner_value, &event.old_value, &event.new_value);
                let start = attr.value_start_byte;
                let end = attr.value_end_byte;
                (start..end, replaced)
            } else {
                let start = attr.value_start_byte;
                let end = attr.value_end_byte;
                (start..end, event.new_value.clone())
            }
        } else {
            // Attribute does not exist on element: insert it right after the tag name
            let insert_pos = matched_elem.start_byte + 1 + matched_elem.tag_name.len();
            let attr_str = format!(" {}=\"{}\"", event.property_or_attr, event.new_value);
            (insert_pos..insert_pos, attr_str)
        };

        let updated_content = format!(
            "{}{}{}",
            &content[..replacement_range.start],
            new_text,
            &content[replacement_range.end..]
        );

        // Compute line number and diff
        let line_starts = Self::compute_line_starts(content);
        let line_no = Self::byte_to_line(&line_starts, replacement_range.start);

        let lines_orig: Vec<&str> = content.lines().collect();
        let lines_new: Vec<&str> = updated_content.lines().collect();

        let old_line_str = lines_orig.get(line_no.saturating_sub(1)).copied().unwrap_or("").to_string();
        let new_line_str = lines_new.get(line_no.saturating_sub(1)).copied().unwrap_or("").to_string();

        let diff = format!(
            "@@ -{},1 +{},1 @@\n- {}\n+ {}",
            line_no,
            line_no,
            old_line_str.trim(),
            new_line_str.trim()
        );

        let hash = blake3::hash(updated_content.as_bytes()).to_hex().to_string();

        if apply_to_disk {
            fs::write(target_file_path, &updated_content).map_err(HgbError::Io)?;
        }

        let rel_path = target_file_path
            .strip_prefix(&self.workspace_root)
            .unwrap_or(target_file_path)
            .to_string_lossy()
            .to_string();

        Ok(Some(TweakSyncReport {
            target_file: rel_path,
            matched_line: line_no,
            old_line: old_line_str,
            new_line: new_line_str,
            diff_applied: diff,
            file_written: apply_to_disk,
            blake3_hash: hash,
            success: true,
            message: format!(
                "AST-aware synced DOM tweak into '{}' (<{}>) at line {}",
                target_file_path.display(),
                matched_elem.tag_name,
                line_no
            ),
        }))
    }

    /// Fallback line-based synchronization
    fn sync_line_fallback(
        &self,
        target_file_path: &Path,
        content: &str,
        event: &DomTweakEvent,
        apply_to_disk: bool,
    ) -> Result<TweakSyncReport> {
        let lines: Vec<&str> = content.lines().collect();
        let mut matched_idx = None;
        let mut old_line_str = String::new();
        let mut new_line_str = String::new();

        for (idx, line) in lines.iter().enumerate() {
            if line.contains(&event.old_value) {
                let matches_context = match (&event.component_hint, &event.selector) {
                    (Some(comp), _) if content.contains(comp.as_str()) => true,
                    (_, sel) if line.contains(sel.trim_start_matches('.')) => true,
                    _ => true,
                };

                if matches_context {
                    matched_idx = Some(idx);
                    old_line_str = line.to_string();
                    new_line_str = line.replace(&event.old_value, &event.new_value);
                    break;
                }
            }
        }

        if matched_idx.is_none() {
            for (idx, line) in lines.iter().enumerate() {
                if line.contains(&event.property_or_attr) {
                    matched_idx = Some(idx);
                    old_line_str = line.to_string();
                    new_line_str = format!("{}: \"{}\"", event.property_or_attr, event.new_value);
                    break;
                }
            }
        }

        let idx = match matched_idx {
            Some(i) => i,
            None => {
                return Ok(TweakSyncReport {
                    target_file: target_file_path.to_string_lossy().to_string(),
                    matched_line: 0,
                    old_line: String::new(),
                    new_line: String::new(),
                    diff_applied: String::new(),
                    file_written: false,
                    blake3_hash: String::new(),
                    success: false,
                    message: format!("Could not locate matching AST target for selector '{}'", event.selector),
                });
            }
        };

        let line_no = idx + 1;
        let diff = format!(
            "@@ -{},1 +{},1 @@\n- {}\n+ {}",
            line_no, line_no, old_line_str.trim(), new_line_str.trim()
        );

        let mut new_lines = lines.clone();
        new_lines[idx] = &new_line_str;
        let updated_content = new_lines.join("\n") + "\n";
        let hash = blake3::hash(updated_content.as_bytes()).to_hex().to_string();

        if apply_to_disk {
            fs::write(target_file_path, &updated_content).map_err(HgbError::Io)?;
        }

        let rel_path = target_file_path
            .strip_prefix(&self.workspace_root)
            .unwrap_or(target_file_path)
            .to_string_lossy()
            .to_string();

        Ok(TweakSyncReport {
            target_file: rel_path,
            matched_line: line_no,
            old_line: old_line_str,
            new_line: new_line_str,
            diff_applied: diff,
            file_written: apply_to_disk,
            blake3_hash: hash,
            success: true,
            message: format!("Successfully synced DOM tweak into '{}' at line {}", target_file_path.display(), line_no),
        })
    }

    /// Score an element against DOM tweak criteria to prevent accidental overwrites
    pub fn score_element(elem: &JsxElement, event: &DomTweakEvent, selector: &ParsedSelector) -> i32 {
        let mut score: i32 = 0;

        // 1. Component Scope Matching
        if let Some(ref comp_hint) = selector.component_scope {
            if let Some(ref scope) = elem.component_scope {
                if scope.eq_ignore_ascii_case(comp_hint) {
                    score += 150;
                } else {
                    score -= 80;
                }
            }
        }

        // 2. Tag Name Matching
        if let Some(ref sel_tag) = selector.tag {
            if elem.tag_name.eq_ignore_ascii_case(sel_tag) {
                score += 100;
            } else {
                // Strict penalty for tag mismatch! (e.g. selector button must not match div)
                score -= 250;
            }
        }

        // 3. ID Matching
        if let Some(ref sel_id) = selector.id {
            if let Some(id_attr) = elem.attributes.iter().find(|a| a.name == "id") {
                if id_attr.inner_value == *sel_id {
                    score += 250;
                } else {
                    score -= 100;
                }
            }
        }

        // 4. Selector Classes Matching
        let elem_class_attr = elem.attributes.iter().find(|a| a.name == "className" || a.name == "class");
        let elem_classes: Vec<&str> = if let Some(ref attr) = elem_class_attr {
            attr.inner_value.split_whitespace().collect()
        } else {
            Vec::new()
        };

        for sel_cls in &selector.classes {
            if elem_classes.contains(&sel_cls.as_str()) {
                score += 45;
            }
        }

        // 5. Target Property / Attribute Match
        let target_attr = elem.attributes.iter().find(|a| a.name.eq_ignore_ascii_case(&event.property_or_attr));
        if let Some(ref attr) = target_attr {
            if attr.inner_value == event.old_value && !event.old_value.is_empty() {
                score += 300;
            } else if attr.inner_value.contains(&event.old_value) && !event.old_value.is_empty() {
                score += 180;
            }

            // Matching class tokens
            if event.property_or_attr.eq_ignore_ascii_case("className") || event.property_or_attr.eq_ignore_ascii_case("class") {
                let old_tokens: Vec<&str> = event.old_value.split_whitespace().collect();
                let matches_count = old_tokens.iter().filter(|t| elem_classes.contains(t)).count();
                score += (matches_count as i32) * 30;
            }
        }

        score
    }

    /// Surgical JSX/TSX element parser
    pub fn parse_jsx_elements(content: &str) -> Vec<JsxElement> {
        let mut elements = Vec::new();
        let bytes = content.as_bytes();
        let len = bytes.len();
        let line_starts = Self::compute_line_starts(content);

        let mut i = 0;
        let mut active_component: Option<String> = None;
        let mut brace_depth: i32 = 0;
        let mut component_depth: i32 = 0;

        while i < len {
            // Track component definitions: `function Name`, `const Name =`, `class Name`
            if bytes[i] == b'f' && content[i..].starts_with("function ") {
                if let Some(name) = Self::extract_identifier(&content[i + 9..]) {
                    if name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                        active_component = Some(name);
                        component_depth = brace_depth;
                    }
                }
            } else if bytes[i] == b'c' && content[i..].starts_with("const ") {
                if let Some(name) = Self::extract_identifier(&content[i + 6..]) {
                    if name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                        active_component = Some(name);
                        component_depth = brace_depth;
                    }
                }
            }

            if bytes[i] == b'{' {
                brace_depth += 1;
            } else if bytes[i] == b'}' {
                brace_depth -= 1;
                if active_component.is_some() && brace_depth <= component_depth {
                    active_component = None;
                }
            }

            // Look for JSX tag start '<'
            if bytes[i] == b'<' && i + 1 < len {
                let next_ch = bytes[i + 1];
                if next_ch.is_ascii_alphabetic() {
                    // Potential JSX tag
                    let tag_start = i;
                    let mut tag_name_end = i + 1;
                    while tag_name_end < len && (bytes[tag_name_end].is_ascii_alphanumeric() || bytes[tag_name_end] == b'.' || bytes[tag_name_end] == b'-' || bytes[tag_name_end] == b'_') {
                        tag_name_end += 1;
                    }

                    let tag_name = String::from_utf8_lossy(&bytes[i + 1..tag_name_end]).to_string();

                    // Parse attributes until '>' or '/>'
                    let mut attr_cursor = tag_name_end;
                    let mut attrs = Vec::new();
                    let mut is_self_closing = false;
                    let mut valid_tag = false;

                    while attr_cursor < len {
                        // Skip whitespace
                        while attr_cursor < len && (bytes[attr_cursor] == b' ' || bytes[attr_cursor] == b'\t' || bytes[attr_cursor] == b'\r' || bytes[attr_cursor] == b'\n') {
                            attr_cursor += 1;
                        }

                        if attr_cursor >= len {
                            break;
                        }

                        if bytes[attr_cursor] == b'/' && attr_cursor + 1 < len && bytes[attr_cursor + 1] == b'>' {
                            is_self_closing = true;
                            attr_cursor += 2;
                            valid_tag = true;
                            break;
                        }

                        if bytes[attr_cursor] == b'>' {
                            attr_cursor += 1;
                            valid_tag = true;
                            break;
                        }

                        // Parse attribute name
                        let attr_name_start = attr_cursor;
                        while attr_cursor < len && (bytes[attr_cursor].is_ascii_alphanumeric() || bytes[attr_cursor] == b'-' || bytes[attr_cursor] == b'_' || bytes[attr_cursor] == b':') {
                            attr_cursor += 1;
                        }

                        if attr_cursor == attr_name_start {
                            // Syntax error or comparison, abort tag
                            break;
                        }

                        let attr_name = String::from_utf8_lossy(&bytes[attr_name_start..attr_cursor]).to_string();

                        // Skip whitespace before '='
                        while attr_cursor < len && (bytes[attr_cursor] == b' ' || bytes[attr_cursor] == b'\t' || bytes[attr_cursor] == b'\r' || bytes[attr_cursor] == b'\n') {
                            attr_cursor += 1;
                        }

                        if attr_cursor < len && bytes[attr_cursor] == b'=' {
                            attr_cursor += 1; // skip '='

                            while attr_cursor < len && (bytes[attr_cursor] == b' ' || bytes[attr_cursor] == b'\t' || bytes[attr_cursor] == b'\r' || bytes[attr_cursor] == b'\n') {
                                attr_cursor += 1;
                            }

                            if attr_cursor < len {
                                if bytes[attr_cursor] == b'"' || bytes[attr_cursor] == b'\'' {
                                    let quote = bytes[attr_cursor];
                                    let val_start = attr_cursor + 1;
                                    attr_cursor += 1;
                                    while attr_cursor < len && bytes[attr_cursor] != quote {
                                        if bytes[attr_cursor] == b'\\' && attr_cursor + 1 < len {
                                            attr_cursor += 2;
                                        } else {
                                            attr_cursor += 1;
                                        }
                                    }
                                    let val_end = attr_cursor;
                                    if attr_cursor < len && bytes[attr_cursor] == quote {
                                        attr_cursor += 1;
                                    }
                                    let raw_val = String::from_utf8_lossy(&bytes[val_start.saturating_sub(1)..attr_cursor]).to_string();
                                    let inner_val = String::from_utf8_lossy(&bytes[val_start..val_end]).to_string();

                                    attrs.push(JsxAttribute {
                                        name: attr_name,
                                        raw_value: raw_val,
                                        inner_value: inner_val,
                                        start_byte: attr_name_start,
                                        end_byte: attr_cursor,
                                        value_start_byte: val_start,
                                        value_end_byte: val_end,
                                    });
                                } else if bytes[attr_cursor] == b'{' {
                                    let val_start = attr_cursor + 1;
                                    attr_cursor += 1;
                                    let mut depth = 1;
                                    while attr_cursor < len && depth > 0 {
                                        if bytes[attr_cursor] == b'{' {
                                            depth += 1;
                                        } else if bytes[attr_cursor] == b'}' {
                                            depth -= 1;
                                        }
                                        attr_cursor += 1;
                                    }
                                    let val_end = attr_cursor.saturating_sub(1);
                                    let raw_val = String::from_utf8_lossy(&bytes[val_start.saturating_sub(1)..attr_cursor]).to_string();
                                    let inner_val = String::from_utf8_lossy(&bytes[val_start..val_end]).to_string();

                                    attrs.push(JsxAttribute {
                                        name: attr_name,
                                        raw_value: raw_val,
                                        inner_value: inner_val,
                                        start_byte: attr_name_start,
                                        end_byte: attr_cursor,
                                        value_start_byte: val_start,
                                        value_end_byte: val_end,
                                    });
                                }
                            }
                        } else {
                            // Boolean attribute
                            attrs.push(JsxAttribute {
                                name: attr_name,
                                raw_value: String::new(),
                                inner_value: String::new(),
                                start_byte: attr_name_start,
                                end_byte: attr_cursor,
                                value_start_byte: attr_cursor,
                                value_end_byte: attr_cursor,
                            });
                        }
                    }

                    if valid_tag {
                        let start_line = Self::byte_to_line(&line_starts, tag_start);
                        let end_line = Self::byte_to_line(&line_starts, attr_cursor);

                        elements.push(JsxElement {
                            tag_name,
                            attributes: attrs,
                            component_scope: active_component.clone(),
                            start_byte: tag_start,
                            end_byte: attr_cursor,
                            start_line,
                            end_line,
                            is_self_closing,
                        });

                        i = attr_cursor;
                        continue;
                    }
                }
            }

            i += 1;
        }

        elements
    }

    fn replace_class_tokens(existing: &str, old_part: &str, new_part: &str) -> String {
        if existing.contains(old_part) && !old_part.is_empty() {
            return existing.replace(old_part, new_part);
        }

        let old_tokens: Vec<&str> = old_part.split_whitespace().collect();
        let new_tokens: Vec<&str> = new_part.split_whitespace().collect();
        let mut existing_tokens: Vec<String> = existing.split_whitespace().map(|s| s.to_string()).collect();

        // Remove old tokens
        existing_tokens.retain(|t| !old_tokens.contains(&t.as_str()));

        // Append new tokens
        for nt in new_tokens {
            if !existing_tokens.iter().any(|t| t == nt) {
                existing_tokens.push(nt.to_string());
            }
        }

        existing_tokens.join(" ")
    }

    fn extract_identifier(s: &str) -> Option<String> {
        let trimmed = s.trim_start();
        let mut ident = String::new();
        for ch in trimmed.chars() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ident.push(ch);
            } else {
                break;
            }
        }
        if ident.is_empty() {
            None
        } else {
            Some(ident)
        }
    }

    fn compute_line_starts(content: &str) -> Vec<usize> {
        let mut starts = vec![0];
        for (idx, b) in content.bytes().enumerate() {
            if b == b'\n' {
                starts.push(idx + 1);
            }
        }
        starts
    }

    fn byte_to_line(starts: &[usize], byte_offset: usize) -> usize {
        match starts.binary_search(&byte_offset) {
            Ok(idx) => idx + 1,
            Err(idx) => idx,
        }
    }

    /// Identify the best matching file candidate
    fn find_candidate_file(&self, event: &DomTweakEvent) -> Result<PathBuf> {
        // 1. Explicit file hint
        if let Some(ref hint) = event.file_hint {
            let p = self.workspace_root.join(hint);
            if p.exists() {
                return Ok(p);
            }
        }

        // 2. Component name hint (e.g. "Header" -> "src/components/Header.tsx")
        if let Some(ref comp) = event.component_hint {
            let extensions = ["tsx", "jsx", "vue", "html", "svelte", "rs"];
            for ext in extensions {
                let candidates = [
                    self.workspace_root.join(format!("src/{}.{}", comp, ext)),
                    self.workspace_root.join(format!("src/components/{}.{}", comp, ext)),
                    self.workspace_root.join(format!("{}.{}", comp, ext)),
                ];
                for c in candidates {
                    if c.exists() {
                        return Ok(c);
                    }
                }
            }
        }

        // 3. Fallback scan src/ directory for files containing the selector or old_value
        let src_dir = self.workspace_root.join("src");
        if src_dir.exists() {
            if let Ok(entries) = fs::read_dir(src_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() {
                        if let Ok(c) = fs::read_to_string(&path) {
                            if c.contains(&event.old_value) {
                                return Ok(path);
                            }
                        }
                    }
                }
            }
        }

        // Default to src/App.tsx or index.html if present
        let default_app = self.workspace_root.join("src/App.tsx");
        if default_app.exists() {
            return Ok(default_app);
        }

        let default_html = self.workspace_root.join("index.html");
        if default_html.exists() {
            return Ok(default_html);
        }

        Err(HgbError::validation(format!(
            "No candidate source file found for selector '{}'",
            event.selector
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ast_element_matching_identical_classes() {
        let tsx_source = r#"
export function Header() {
  return (
    <header className="flex items-center justify-between p-4">
      <div className="flex items-center space-x-2">Logo</div>
      <button className="flex items-center text-sm text-gray-700">Sign In</button>
    </header>
  );
}

export function Sidebar() {
  return (
    <aside className="w-64">
      <button className="flex items-center text-sm text-blue-600">Dashboard</button>
    </aside>
  );
}
"#;
        let tmp = std::env::temp_dir().join(format!("cdp_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let _ = fs::create_dir_all(&tmp);
        let test_file = tmp.join("App.tsx");
        fs::write(&test_file, tsx_source).unwrap();

        let mirror = CdpTweakMirror::new(&tmp);

        // Target the button in Sidebar specifically
        let event = DomTweakEvent {
            selector: "button.text-blue-600".to_string(),
            property_or_attr: "className".to_string(),
            old_value: "text-blue-600".to_string(),
            new_value: "text-blue-800 font-bold".to_string(),
            component_hint: Some("Sidebar".to_string()),
            file_hint: Some("App.tsx".to_string()),
        };

        let report = mirror.sync_tweak(&event, true).expect("sync tweak");
        assert!(report.success);

        let updated = fs::read_to_string(&test_file).unwrap();

        // Verify Sidebar button was updated
        assert!(updated.contains("text-blue-800 font-bold"));
        // Verify Header button was NOT touched
        assert!(updated.contains("className=\"flex items-center text-sm text-gray-700\""));
        // Verify Header div was NOT touched
        assert!(updated.contains("<div className=\"flex items-center space-x-2\">"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_ast_multiline_jsx_element_sync() {
        let tsx_source = r#"
export function CheckoutButton() {
  return (
    <button
      id="checkout-btn"
      className="flex items-center bg-blue-500 hover:bg-blue-600 px-4 py-2"
      disabled={false}
    >
      Proceed
    </button>
  );
}
"#;
        let tmp = std::env::temp_dir().join(format!("cdp_multi_test_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let _ = fs::create_dir_all(&tmp);
        let test_file = tmp.join("Checkout.tsx");
        fs::write(&test_file, tsx_source).unwrap();

        let mirror = CdpTweakMirror::new(&tmp);

        let event = DomTweakEvent {
            selector: "button#checkout-btn".to_string(),
            property_or_attr: "className".to_string(),
            old_value: "bg-blue-500".to_string(),
            new_value: "bg-emerald-600".to_string(),
            component_hint: Some("CheckoutButton".to_string()),
            file_hint: Some("Checkout.tsx".to_string()),
        };

        let report = mirror.sync_tweak(&event, true).expect("sync multiline");
        assert!(report.success);

        let updated = fs::read_to_string(&test_file).unwrap();
        assert!(updated.contains("bg-emerald-600 hover:bg-blue-600"));
        assert!(updated.contains("id=\"checkout-btn\""));
        assert!(updated.contains("disabled={false}"));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_inspect_dom_element_and_apply_visual_patch() {
        let tsx_source = r#"export function Hero() {
  return (
    <div className="flex flex-col items-center">
      <h1 id="main-title" className="text-4xl font-bold text-cyan-400">Welcome</h1>
      <button
        id="cta-btn"
        className="px-6 py-3 bg-cyan-500 rounded-lg hover:bg-cyan-400"
        aria-label="Get Started"
      >
        Get Started
      </button>
    </div>
  );
}
"#;
        let mirror = CdpTweakMirror::new("/tmp");

        // 1. Inspect DOM element
        let inspected = mirror.inspect_dom_element(tsx_source, "button#cta-btn").expect("element found");
        assert_eq!(inspected.tag_name, "button");
        assert_eq!(inspected.id, Some("cta-btn".to_string()));
        assert!(inspected.classes.contains(&"px-6".to_string()));
        assert!(inspected.classes.contains(&"bg-cyan-500".to_string()));
        assert_eq!(inspected.attributes.get("aria-label").map(|s| s.as_str()), Some("Get Started"));
        assert_eq!(inspected.indentation, "      "); // 6 spaces indentation preserved

        // 2. Apply visual patch: add class, remove class, update attribute
        let mut attr_updates = std::collections::HashMap::new();
        attr_updates.insert("aria-label".to_string(), "Launch App Now".to_string());

        let patched = mirror.apply_visual_patch(
            tsx_source,
            "button#cta-btn",
            &["shadow-2xl".to_string(), "bg-emerald-600".to_string()],
            &["bg-cyan-500".to_string()],
            &attr_updates,
        ).expect("patch applied");

        assert!(patched.contains("Launch App Now"));
        assert!(patched.contains("shadow-2xl"));
        assert!(patched.contains("bg-emerald-600"));
        assert!(!patched.contains("bg-cyan-500"));

        // Verify surrounding indentation is intact
        assert!(patched.contains("      <button"));
        assert!(patched.contains("        id=\"cta-btn\""));
    }
}
