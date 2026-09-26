use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use hgb_core::ambient_ast::AmbientAstFollower;
use hgb_core::error::{HgbError, Result};

/// CDP / DOM Click event payload coming from browser extension or devtools bridge
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DomElementClickEvent {
    pub selector: String,
    pub tag_name: String,
    pub inner_text: Option<String>,
    pub react_source_file: Option<String>,
    pub react_source_line: Option<usize>,
    pub vite_sourcemap_ref: Option<String>,
}

/// Teleport resolution report tying a live DOM element directly to source AST
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TeleportResult {
    pub resolved_file: PathBuf,
    pub line_number: usize,
    pub enclosing_symbol: Option<String>,
    pub symbol_kind: Option<String>,
    pub ambient_snippet: String,
    pub prompt_anchor: String,
}

/// Click-to-Code DOM-to-AST Visual Teleporter
pub struct DomToAstTeleporter {
    workspace_root: PathBuf,
    follower: AmbientAstFollower,
}

impl DomToAstTeleporter {
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        let ws = workspace_root.into();
        let follower = AmbientAstFollower::new(&ws);
        Self {
            workspace_root: ws,
            follower,
        }
    }

    /// Ingest raw JSON event emitted by Chrome DevTools Protocol or Vite client
    pub fn ingest_click_event(&mut self, json_str: &str) -> Result<TeleportResult> {
        let event: DomElementClickEvent = serde_json::from_str(json_str)
            .map_err(|e| HgbError::validation(format!("Failed to parse DOM click event: {}", e)))?;

        self.teleport_from_event(&event)
    }

    /// Resolve DOM event to exact source file and enclosing AST symbol
    pub fn teleport_from_event(&mut self, event: &DomElementClickEvent) -> Result<TeleportResult> {
        let (file_str, line) = if let (Some(f), Some(l)) = (&event.react_source_file, event.react_source_line) {
            (f.clone(), l)
        } else if let Some(vite_ref) = &event.vite_sourcemap_ref {
            // Parse "src/components/Button.tsx:45:12"
            let parts: Vec<&str> = vite_ref.split(':').collect();
            let f = parts.first().unwrap_or(&"src/App.tsx").to_string();
            let l = parts.get(1).and_then(|s| s.parse::<usize>().ok()).unwrap_or(1);
            (f, l)
        } else {
            // Heuristic fallback: attempt to find file matching selector or tag
            let fallback_file = self.discover_file_for_selector(&event.selector)
                .unwrap_or_else(|| "src/App.tsx".to_string());
            (fallback_file, 1)
        };

        // Normalize path
        let rel_path = file_str.trim_start_matches('/').trim_start_matches("./");
        let resolved = self.workspace_root.join(rel_path);

        // Update ambient follower
        self.follower.set_focus(rel_path, line);
        let ctx = self.follower.get_ambient_context();

        let prompt_anchor = format!(
            "<dom_teleport_anchor>\n\
             Selected Element: <{} class=\"{}\">\n\
             Element Text: \"{}\"\n\
             Source Target: {}:{}\n\
             Enclosing Symbol: {} ({})\n\
             Context Snippet:\n\
             ```\n\
             {}\n\
             ```\n\
             </dom_teleport_anchor>",
            event.tag_name,
            event.selector,
            event.inner_text.as_deref().unwrap_or(""),
            rel_path,
            line,
            ctx.enclosing_symbol.as_deref().unwrap_or("global"),
            ctx.symbol_kind.as_deref().unwrap_or("file"),
            ctx.context_snippet
        );

        Ok(TeleportResult {
            resolved_file: resolved,
            line_number: line,
            enclosing_symbol: ctx.enclosing_symbol,
            symbol_kind: ctx.symbol_kind,
            ambient_snippet: ctx.context_snippet,
            prompt_anchor,
        })
    }

    fn discover_file_for_selector(&self, selector: &str) -> Option<String> {
        // Strip CSS classes or IDs to find component names like "checkout-btn" -> "Checkout"
        let token = selector.split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
            .find(|s| !s.is_empty() && *s != "div" && *s != "button" && *s != "span")?;

        let candidates = [
            format!("src/{}.tsx", token),
            format!("src/components/{}.tsx", token),
            format!("src/{}.rs", token),
        ];

        for c in &candidates {
            if self.workspace_root.join(c).exists() {
                return Some(c.clone());
            }
        }
        None
    }
}
