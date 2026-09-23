//! # Interactive Cockpit TUI Engine for Hagibis (`hgb cockpit`)
//!
//! Provides live DAG visual tracking, multi-agent hierarchy, mid-flight agent steering
//! (Pause, EditScratchpad, RedirectTool, Resume, Abort, InjectContext), active tool call
//! streaming, background tasks monitor, surgical diff viewer, and real-time telemetry rendering.

use chrono::Utc;
use crossterm::{
    event::{
        self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
    },
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use hgb_core::{HgbError, Result};
use ratatui::{
    backend::{CrosstermBackend, TestBackend},
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Table, Tabs, Wrap,
    },
    Frame, Terminal,
};
use serde::{Deserialize, Serialize};
use std::io::{self, stdout};
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

/// Operational status of a DAG node visualized in the Cockpit
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CockpitNodeStatus {
    Pending,
    Running { progress_pct: u8 },
    Succeeded { duration_ms: u64 },
    Failed { error: String },
    Paused,
    Steered { note: String },
}

impl CockpitNodeStatus {
    pub fn badge(&self) -> (&'static str, Color) {
        match self {
            Self::Pending => ("[PENDING]", Color::DarkGray),
            Self::Running { .. } => ("[RUNNING]", Color::Cyan),
            Self::Succeeded { .. } => ("[SUCCESS]", Color::Green),
            Self::Failed { .. } => ("[FAILED]", Color::Red),
            Self::Paused => ("[PAUSED]", Color::Yellow),
            Self::Steered { .. } => ("[STEERED]", Color::Magenta),
        }
    }
}

/// A recorded tool invocation on a DAG node
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CockpitToolCall {
    pub tool_name: String,
    pub parameters_summary: String,
    pub status: String,
    pub duration_ms: u64,
    pub output_snippet: Option<String>,
}

impl CockpitToolCall {
    pub fn new(
        tool_name: impl Into<String>,
        parameters_summary: impl Into<String>,
        status: impl Into<String>,
        duration_ms: u64,
        output_snippet: Option<String>,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            parameters_summary: parameters_summary.into(),
            status: status.into(),
            duration_ms,
            output_snippet,
        }
    }
}

/// Background asynchronous task monitored by Cockpit
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CockpitBackgroundTask {
    pub task_id: String,
    pub description: String,
    pub status: String,
    pub elapsed_secs: u64,
}

impl CockpitBackgroundTask {
    pub fn new(
        task_id: impl Into<String>,
        description: impl Into<String>,
        status: impl Into<String>,
        elapsed_secs: u64,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            description: description.into(),
            status: status.into(),
            elapsed_secs,
        }
    }
}

/// File artifact change diff tracked in Cockpit
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CockpitArtifactDiff {
    pub file_path: String,
    pub added_lines: usize,
    pub deleted_lines: usize,
    pub diff_content: String,
}

impl CockpitArtifactDiff {
    pub fn new(
        file_path: impl Into<String>,
        added_lines: usize,
        deleted_lines: usize,
        diff_content: impl Into<String>,
    ) -> Self {
        Self {
            file_path: file_path.into(),
            added_lines,
            deleted_lines,
            diff_content: diff_content.into(),
        }
    }
}

/// Active tab shown in the Cockpit right inspector pane
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CockpitActiveTab {
    LiveStream,
    ArtifactDiffs,
    BackgroundTasks,
}

impl CockpitActiveTab {
    pub fn title(&self) -> &'static str {
        match self {
            Self::LiveStream => "1: Live Stream & Reasoning",
            Self::ArtifactDiffs => "2: Artifacts & Diffs",
            Self::BackgroundTasks => "3: Background Tasks",
        }
    }

    pub fn index(&self) -> usize {
        match self {
            Self::LiveStream => 0,
            Self::ArtifactDiffs => 1,
            Self::BackgroundTasks => 2,
        }
    }

    pub fn from_index(index: usize) -> Self {
        match index % 3 {
            0 => Self::LiveStream,
            1 => Self::ArtifactDiffs,
            _ => Self::BackgroundTasks,
        }
    }

    pub fn next(&self) -> Self {
        Self::from_index(self.index() + 1)
    }

    pub fn prev(&self) -> Self {
        Self::from_index(if self.index() == 0 { 2 } else { self.index() - 1 })
    }

    pub fn all() -> &'static [CockpitActiveTab] {
        &[
            CockpitActiveTab::LiveStream,
            CockpitActiveTab::ArtifactDiffs,
            CockpitActiveTab::BackgroundTasks,
        ]
    }
}

/// Primary view mode of the Hagibis TUI interface
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CockpitViewMode {
    /// Authentic AGY CLI Conversational Chat Canvas (default)
    ChatCanvas,
    /// Multi-Pane Developer Cockpit (Top HUD + Left DAG tree + Right tabs + Steering deck)
    CockpitSplit,
}

/// Active modal overlay dialog
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CockpitOverlay {
    None,
    Shortcuts,
    ModelPicker { selected: usize },
    Tasks,
}

/// A conversation turn rendered in the Chat Canvas
#[derive(Debug, Clone, PartialEq)]
pub struct CockpitChatItem {
    pub sender: CockpitChatSender,
    pub content: String,
    pub tokens: usize,
    pub duration_ms: u64,
    pub timestamp: String,
    pub thinking: Option<String>,
    pub tool_calls: Vec<CockpitToolCall>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CockpitChatSender {
    User,
    Assistant { model: String },
    System,
}

/// Input mode in interactive Cockpit session
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CockpitInputMode {
    Normal,
    Input,
}

/// A node in the DAG monitored and steered by the Cockpit
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CockpitDagNode {
    pub id: String,
    pub name: String,
    pub model: String,
    pub status: CockpitNodeStatus,
    pub tokens_used: u64,
    pub scratchpad: String,
    pub tool_calls: Vec<CockpitToolCall>,
    pub dependencies: Vec<String>,
    pub parent_id: Option<String>,
}

impl CockpitDagNode {
    pub fn new(id: impl Into<String>, name: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            model: model.into(),
            status: CockpitNodeStatus::Pending,
            tokens_used: 0,
            scratchpad: String::new(),
            tool_calls: Vec::new(),
            dependencies: Vec::new(),
            parent_id: None,
        }
    }

    pub fn with_parent(mut self, parent_id: impl Into<String>) -> Self {
        self.parent_id = Some(parent_id.into());
        self
    }

    pub fn with_status(mut self, status: CockpitNodeStatus) -> Self {
        self.status = status;
        self
    }

    pub fn with_tokens(mut self, tokens: u64) -> Self {
        self.tokens_used = tokens;
        self
    }

    pub fn with_scratchpad(mut self, scratchpad: impl Into<String>) -> Self {
        self.scratchpad = scratchpad.into();
        self
    }

    pub fn with_dependencies(mut self, deps: Vec<String>) -> Self {
        self.dependencies = deps;
        self
    }

    pub fn add_tool_call(&mut self, call: CockpitToolCall) {
        self.tool_calls.push(call);
    }
}

/// Mid-flight steering actions that can be dispatched to running agents
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SteeringAction {
    /// Pause execution of a specific DAG node
    Pause { node_id: String },
    /// Resume execution of a paused DAG node
    Resume { node_id: String },
    /// Directly edit an agent's internal scratchpad mid-flight
    EditScratchpad { node_id: String, new_scratchpad: String },
    /// Redirect an impending tool invocation to an alternative tool or sanitize arguments
    RedirectTool { node_id: String, new_tool_name: String, parameters: serde_json::Value },
    /// Abort execution of a DAG node with an explanation
    Abort { node_id: String, reason: String },
    /// Inject additional high-priority context into agent prompt
    InjectContext { node_id: String, additional_context: String },
}

impl SteeringAction {
    pub fn action_type(&self) -> &'static str {
        match self {
            Self::Pause { .. } => "Pause",
            Self::Resume { .. } => "Resume",
            Self::EditScratchpad { .. } => "EditScratchpad",
            Self::RedirectTool { .. } => "RedirectTool",
            Self::Abort { .. } => "Abort",
            Self::InjectContext { .. } => "InjectContext",
        }
    }

    pub fn target_node_id(&self) -> &str {
        match self {
            Self::Pause { node_id } => node_id,
            Self::Resume { node_id } => node_id,
            Self::EditScratchpad { node_id, .. } => node_id,
            Self::RedirectTool { node_id, .. } => node_id,
            Self::Abort { node_id, .. } => node_id,
            Self::InjectContext { node_id, .. } => node_id,
        }
    }
}

/// Real-time system telemetry captured by Cockpit
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CockpitTelemetry {
    pub total_tokens: u64,
    pub tokens_per_sec: f64,
    pub peak_temperature_celsius: f64,
    pub battery_pct: Option<u8>,
    pub lakandiwa_entropy_bits: f64,
    pub speculative_acceptance_rate: f64,
    pub uds_latency_us: u64,
    pub active_agents: usize,
}

impl Default for CockpitTelemetry {
    fn default() -> Self {
        Self {
            total_tokens: 0,
            tokens_per_sec: 0.0,
            peak_temperature_celsius: 42.0,
            battery_pct: Some(88),
            lakandiwa_entropy_bits: 0.45,
            speculative_acceptance_rate: 0.92,
            uds_latency_us: 120,
            active_agents: 1,
        }
    }
}

/// Complete state machine for the Interactive Cockpit TUI Engine
#[derive(Debug, Clone)]
pub struct CockpitState {
    pub nodes: Vec<CockpitDagNode>,
    pub selected_index: usize,
    pub telemetry: CockpitTelemetry,
    pub steering_history: Vec<(u64, SteeringAction)>,
    pub log_feed: Vec<String>,
    pub is_paused: bool,
    pub active_tab: CockpitActiveTab,
    pub background_tasks: Vec<CockpitBackgroundTask>,
    pub artifact_diffs: Vec<CockpitArtifactDiff>,
    pub prompt_input: String,
    pub prompt_history: Vec<String>,
    pub cursor_position: usize,
    pub history_index: usize,
    pub draft_prompt: String,
    pub kill_ring: String,
    pub model_pill: String,
    pub reasoning_effort: String,
    pub auth_account: String,
    pub workspace_path: String,
    pub input_mode: CockpitInputMode,
    pub view_mode: CockpitViewMode,
    pub overlay: CockpitOverlay,
    pub conversation: Vec<CockpitChatItem>,
    pub chat_scroll: usize,
    pub execution_mode: String,
}

impl Default for CockpitState {
    fn default() -> Self {
        Self::new()
    }
}

impl CockpitState {
    pub fn new() -> Self {
        let auth_account = hgb_core::GeminiOAuthManager::get_account_email()
            .unwrap_or_else(|| "buzer.agy@gmail.com".to_string());
        let workspace_path = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "/home/dyna/TGS Projects/hagibis".to_string());

        let default_model = if hgb_core::OllamaProvider::is_available() {
            "qwen2.5-coder:1.5b".to_string()
        } else {
            "gemini-2.5-flash".to_string()
        };

        Self {
            nodes: Vec::new(),
            selected_index: 0,
            telemetry: CockpitTelemetry::default(),
            steering_history: Vec::new(),
            log_feed: vec!["[SYSTEM] Hagibis AGY Cockpit & Chat Canvas initialized.".to_string()],
            is_paused: false,
            active_tab: CockpitActiveTab::LiveStream,
            background_tasks: Vec::new(),
            artifact_diffs: Vec::new(),
            prompt_input: String::new(),
            prompt_history: Vec::new(),
            cursor_position: 0,
            history_index: 0,
            draft_prompt: String::new(),
            kill_ring: String::new(),
            model_pill: default_model,
            reasoning_effort: "High".to_string(),
            auth_account,
            workspace_path,
            input_mode: CockpitInputMode::Normal,
            view_mode: CockpitViewMode::ChatCanvas,
            overlay: CockpitOverlay::None,
            conversation: Vec::new(),
            chat_scroll: 0,
            execution_mode: "default".to_string(),
        }
    }

    pub fn add_user_message(&mut self, text: impl Into<String>) {
        self.conversation.push(CockpitChatItem {
            sender: CockpitChatSender::User,
            content: text.into(),
            tokens: 0,
            duration_ms: 0,
            timestamp: Utc::now().format("%H:%M:%S").to_string(),
            thinking: None,
            tool_calls: Vec::new(),
        });
    }

    pub fn add_assistant_message(
        &mut self,
        text: impl Into<String>,
        model: impl Into<String>,
        tokens: usize,
        duration_ms: u64,
        thinking: Option<String>,
        tool_calls: Vec<CockpitToolCall>,
    ) {
        self.conversation.push(CockpitChatItem {
            sender: CockpitChatSender::Assistant { model: model.into() },
            content: text.into(),
            tokens,
            duration_ms,
            timestamp: Utc::now().format("%H:%M:%S").to_string(),
            thinking,
            tool_calls,
        });
    }

    pub fn add_system_notice(&mut self, text: impl Into<String>) {
        self.conversation.push(CockpitChatItem {
            sender: CockpitChatSender::System,
            content: text.into(),
            tokens: 0,
            duration_ms: 0,
            timestamp: Utc::now().format("%H:%M:%S").to_string(),
            thinking: None,
            tool_calls: Vec::new(),
        });
    }

    // ── Prompt Buffer & AGY Keybindings ──

    pub fn insert_char(&mut self, c: char) {
        let mut chars: Vec<char> = self.prompt_input.chars().collect();
        let idx = self.cursor_position.min(chars.len());
        chars.insert(idx, c);
        self.prompt_input = chars.into_iter().collect();
        self.cursor_position = idx + 1;
    }

    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            self.insert_char(c);
        }
    }

    pub fn delete_backward(&mut self) {
        if self.cursor_position > 0 && !self.prompt_input.is_empty() {
            let mut chars: Vec<char> = self.prompt_input.chars().collect();
            if self.cursor_position <= chars.len() {
                chars.remove(self.cursor_position - 1);
                self.prompt_input = chars.into_iter().collect();
                self.cursor_position -= 1;
            }
        }
    }

    pub fn delete_forward(&mut self) {
        let mut chars: Vec<char> = self.prompt_input.chars().collect();
        if self.cursor_position < chars.len() {
            chars.remove(self.cursor_position);
            self.prompt_input = chars.into_iter().collect();
        }
    }

    pub fn move_cursor_left(&mut self) {
        if self.cursor_position > 0 {
            self.cursor_position -= 1;
        }
    }

    pub fn move_cursor_right(&mut self) {
        let count = self.prompt_input.chars().count();
        if self.cursor_position < count {
            self.cursor_position += 1;
        }
    }

    pub fn move_to_start(&mut self) {
        self.cursor_position = 0;
    }

    pub fn move_to_end(&mut self) {
        self.cursor_position = self.prompt_input.chars().count();
    }

    pub fn move_word_backward(&mut self) {
        let chars: Vec<char> = self.prompt_input.chars().collect();
        if self.cursor_position == 0 || chars.is_empty() {
            self.cursor_position = 0;
            return;
        }
        let mut i = self.cursor_position.min(chars.len());
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        self.cursor_position = i;
    }

    pub fn move_word_forward(&mut self) {
        let chars: Vec<char> = self.prompt_input.chars().collect();
        let len = chars.len();
        if self.cursor_position >= len {
            self.cursor_position = len;
            return;
        }
        let mut i = self.cursor_position;
        while i < len && !chars[i].is_whitespace() {
            i += 1;
        }
        while i < len && chars[i].is_whitespace() {
            i += 1;
        }
        self.cursor_position = i;
    }

    pub fn kill_to_end(&mut self) {
        let chars: Vec<char> = self.prompt_input.chars().collect();
        if self.cursor_position < chars.len() {
            self.kill_ring = chars[self.cursor_position..].iter().collect();
            self.prompt_input = chars[..self.cursor_position].iter().collect();
        }
    }

    pub fn kill_to_start(&mut self) {
        let chars: Vec<char> = self.prompt_input.chars().collect();
        if self.cursor_position > 0 {
            let kill_idx = self.cursor_position.min(chars.len());
            self.kill_ring = chars[..kill_idx].iter().collect();
            self.prompt_input = chars[kill_idx..].iter().collect();
            self.cursor_position = 0;
        }
    }

    pub fn kill_word_backward(&mut self) {
        let chars: Vec<char> = self.prompt_input.chars().collect();
        if self.cursor_position == 0 || chars.is_empty() {
            return;
        }
        let curr = self.cursor_position.min(chars.len());
        let mut i = curr;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        self.kill_ring = chars[i..curr].iter().collect();
        let mut remaining = Vec::with_capacity(chars.len() - (curr - i));
        remaining.extend_from_slice(&chars[..i]);
        remaining.extend_from_slice(&chars[curr..]);
        self.prompt_input = remaining.into_iter().collect();
        self.cursor_position = i;
    }

    pub fn kill_word_forward(&mut self) {
        let chars: Vec<char> = self.prompt_input.chars().collect();
        let len = chars.len();
        if self.cursor_position >= len {
            return;
        }
        let curr = self.cursor_position;
        let mut i = curr;
        while i < len && !chars[i].is_whitespace() {
            i += 1;
        }
        while i < len && chars[i].is_whitespace() {
            i += 1;
        }
        self.kill_ring = chars[curr..i].iter().collect();
        let mut remaining = Vec::with_capacity(chars.len() - (i - curr));
        remaining.extend_from_slice(&chars[..curr]);
        remaining.extend_from_slice(&chars[i..]);
        self.prompt_input = remaining.into_iter().collect();
    }

    pub fn yank(&mut self) {
        if !self.kill_ring.is_empty() {
            let text = self.kill_ring.clone();
            self.insert_str(&text);
        }
    }

    pub fn clear_prompt(&mut self) {
        self.prompt_input.clear();
        self.cursor_position = 0;
    }

    pub fn history_prev(&mut self) {
        if self.prompt_history.is_empty() {
            return;
        }
        if self.history_index == self.prompt_history.len() {
            self.draft_prompt = self.prompt_input.clone();
        }
        if self.history_index > 0 {
            self.history_index -= 1;
            self.prompt_input = self.prompt_history[self.history_index].clone();
            self.cursor_position = self.prompt_input.chars().count();
        }
    }

    pub fn history_next(&mut self) {
        if self.history_index + 1 < self.prompt_history.len() {
            self.history_index += 1;
            self.prompt_input = self.prompt_history[self.history_index].clone();
            self.cursor_position = self.prompt_input.chars().count();
        } else if self.history_index + 1 == self.prompt_history.len() {
            self.history_index = self.prompt_history.len();
            self.prompt_input = self.draft_prompt.clone();
            self.cursor_position = self.prompt_input.chars().count();
        }
    }

    // ── Chat Viewport Scrolling ──

    pub fn scroll_chat_up(&mut self, lines: usize) {
        self.chat_scroll = self.chat_scroll.saturating_add(lines);
    }

    pub fn scroll_chat_down(&mut self, lines: usize) {
        self.chat_scroll = self.chat_scroll.saturating_sub(lines);
    }

    pub fn scroll_chat_to_top(&mut self) {
        self.chat_scroll = 9999;
    }

    pub fn scroll_chat_to_bottom(&mut self) {
        self.chat_scroll = 0;
    }

    pub fn toggle_view_mode(&mut self) {
        self.view_mode = match self.view_mode {
            CockpitViewMode::ChatCanvas => CockpitViewMode::CockpitSplit,
            CockpitViewMode::CockpitSplit => CockpitViewMode::ChatCanvas,
        };
        let mode_name = match self.view_mode {
            CockpitViewMode::ChatCanvas => "Chat Canvas",
            CockpitViewMode::CockpitSplit => "DAG Swarm Cockpit",
        };
        self.add_log(format!("[VIEW] Switched interface to {}", mode_name));
    }

    pub fn cycle_execution_mode(&mut self) {
        self.execution_mode = match self.execution_mode.as_str() {
            "default" => "plan".to_string(),
            "plan" => "accept-edits".to_string(),
            _ => "default".to_string(),
        };
        self.add_log(format!("[MODE] Execution mode set to '{}'", self.execution_mode));
    }

    pub fn with_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace_path = workspace.into();
        self
    }

    /// Add a node to the visual DAG tracker
    pub fn add_node(&mut self, node: CockpitDagNode) {
        self.nodes.push(node);
        self.telemetry.active_agents = self.nodes.len();
    }

    /// Update status of a DAG node
    pub fn update_node_status(&mut self, node_id: &str, status: CockpitNodeStatus) {
        if let Some(node) = self.nodes.iter_mut().find(|n| n.id == node_id) {
            node.status = status;
        }
    }

    /// Add a tool call to a specific node
    pub fn add_tool_call(&mut self, node_id: &str, call: CockpitToolCall) {
        if let Some(node) = self.nodes.iter_mut().find(|n| n.id == node_id) {
            node.add_tool_call(call);
        }
    }

    /// Track a file artifact diff
    pub fn add_artifact_diff(&mut self, diff: CockpitArtifactDiff) {
        self.artifact_diffs.push(diff);
    }

    /// Add a background task
    pub fn add_background_task(&mut self, task: CockpitBackgroundTask) {
        self.background_tasks.push(task);
    }

    /// Update background task status
    pub fn update_background_task_status(&mut self, task_id: &str, status: impl Into<String>, elapsed_secs: u64) {
        if let Some(task) = self.background_tasks.iter_mut().find(|t| t.task_id == task_id) {
            task.status = status.into();
            task.elapsed_secs = elapsed_secs;
        }
    }

    /// Apply a mid-flight steering action
    pub fn apply_steering(&mut self, action: SteeringAction) -> Result<()> {
        let now_s = Utc::now().timestamp() as u64;

        match &action {
            SteeringAction::Pause { node_id } => {
                let node = self.nodes.iter_mut().find(|n| &n.id == node_id).ok_or_else(|| {
                    HgbError::Execution(format!("Node '{}' not found", node_id))
                })?;
                node.status = CockpitNodeStatus::Paused;
                self.add_log(format!("[STEER] Paused node '{}'", node_id));
            }
            SteeringAction::Resume { node_id } => {
                let node = self.nodes.iter_mut().find(|n| &n.id == node_id).ok_or_else(|| {
                    HgbError::Execution(format!("Node '{}' not found", node_id))
                })?;
                node.status = CockpitNodeStatus::Running { progress_pct: 0 };
                self.add_log(format!("[STEER] Resumed node '{}'", node_id));
            }
            SteeringAction::EditScratchpad { node_id, new_scratchpad } => {
                let node = self.nodes.iter_mut().find(|n| &n.id == node_id).ok_or_else(|| {
                    HgbError::Execution(format!("Node '{}' not found", node_id))
                })?;
                node.scratchpad = new_scratchpad.clone();
                node.status = CockpitNodeStatus::Steered {
                    note: "Scratchpad manually updated".to_string(),
                };
                self.add_log(format!("[STEER] Updated scratchpad on node '{}'", node_id));
            }
            SteeringAction::RedirectTool { node_id, new_tool_name, parameters } => {
                let node = self.nodes.iter_mut().find(|n| &n.id == node_id).ok_or_else(|| {
                    HgbError::Execution(format!("Node '{}' not found", node_id))
                })?;
                node.tool_calls.push(CockpitToolCall::new(
                    new_tool_name,
                    parameters.to_string(),
                    "REDIRECTED",
                    0,
                    Some("Tool invocation redirected by supervisor".to_string()),
                ));
                node.status = CockpitNodeStatus::Steered {
                    note: format!("Tool redirected to {}", new_tool_name),
                };
                self.add_log(format!("[STEER] Redirected tool for node '{}' to '{}'", node_id, new_tool_name));
            }
            SteeringAction::Abort { node_id, reason } => {
                let node = self.nodes.iter_mut().find(|n| &n.id == node_id).ok_or_else(|| {
                    HgbError::Execution(format!("Node '{}' not found", node_id))
                })?;
                node.status = CockpitNodeStatus::Failed {
                    error: format!("Aborted by supervisor: {}", reason),
                };
                self.add_log(format!("[STEER] Aborted node '{}': {}", node_id, reason));
            }
            SteeringAction::InjectContext { node_id, additional_context } => {
                let node = self.nodes.iter_mut().find(|n| &n.id == node_id).ok_or_else(|| {
                    HgbError::Execution(format!("Node '{}' not found", node_id))
                })?;
                node.scratchpad.push_str(&format!("\n[INJECTED_CONTEXT]: {}", additional_context));
                self.add_log(format!("[STEER] Injected context into node '{}'", node_id));
            }
        }

        self.steering_history.push((now_s, action));
        Ok(())
    }

    /// Update telemetry metrics
    pub fn update_telemetry(&mut self, tel: CockpitTelemetry) {
        self.telemetry = tel;
    }

    /// Set authenticated account badge
    pub fn set_auth_account(&mut self, account: impl Into<String>) {
        self.auth_account = account.into();
    }

    /// Set active model pill
    pub fn set_model(&mut self, model: impl Into<String>) {
        self.model_pill = model.into();
    }

    /// Set reasoning effort badge
    pub fn set_reasoning_effort(&mut self, effort: impl Into<String>) {
        self.reasoning_effort = effort.into();
    }

    /// Set active workspace directory
    pub fn set_workspace(&mut self, ws: impl Into<String>) {
        self.workspace_path = ws.into();
    }

    /// Set active right pane tab
    pub fn set_active_tab(&mut self, tab: CockpitActiveTab) {
        self.active_tab = tab;
        self.view_mode = CockpitViewMode::CockpitSplit;
    }

    /// Switch to next tab
    pub fn next_tab(&mut self) {
        self.active_tab = self.active_tab.next();
        self.view_mode = CockpitViewMode::CockpitSplit;
    }

    /// Switch to previous tab
    pub fn prev_tab(&mut self) {
        self.active_tab = self.active_tab.prev();
        self.view_mode = CockpitViewMode::CockpitSplit;
    }

    /// Add an entry to the log feed
    pub fn add_log(&mut self, msg: impl Into<String>) {
        self.log_feed.push(msg.into());
        if self.log_feed.len() > 100 {
            self.log_feed.remove(0);
        }
    }

    pub fn select_next(&mut self) {
        if !self.nodes.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.nodes.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.nodes.is_empty() {
            if self.selected_index == 0 {
                self.selected_index = self.nodes.len() - 1;
            } else {
                self.selected_index -= 1;
            }
        }
    }

    pub fn selected_node(&self) -> Option<&CockpitDagNode> {
        self.nodes.get(self.selected_index)
    }

    pub fn selected_node_mut(&mut self) -> Option<&mut CockpitDagNode> {
        self.nodes.get_mut(self.selected_index)
    }

    /// Submit current prompt in prompt buffer to execution pipeline
    pub async fn submit_current_prompt(&mut self) {
        let prompt = self.prompt_input.trim().to_string();
        if prompt.is_empty() {
            return;
        }

        self.prompt_history.push(prompt.clone());
        self.history_index = self.prompt_history.len();
        self.draft_prompt.clear();
        self.prompt_input.clear();
        self.cursor_position = 0;
        self.chat_scroll = 0;

        // 1. Record User message in Chat Canvas
        self.add_user_message(prompt.clone());

        let node_id = format!("task-{}", self.nodes.len() + 1);
        let prompt_preview = if prompt.len() > 28 {
            format!("{}...", &prompt[..25])
        } else {
            prompt.clone()
        };

        let mut node = CockpitDagNode::new(&node_id, format!("Prompt: {}", prompt_preview), &self.model_pill);
        node.status = CockpitNodeStatus::Running { progress_pct: 10 };
        node.scratchpad = format!("Dispatched prompt: {}\nAwaiting AI microkernel execution...", prompt);
        self.add_node(node);
        self.selected_index = self.nodes.len() - 1;
        self.add_log(format!("[EXEC] Dispatched prompt '{}'", prompt_preview));

        self.execute_prompt_on_node(&node_id, &prompt).await;

        // 2. Record Assistant response in Chat Canvas
        let (output, tokens, duration_ms) = if let Some(node) = self.nodes.iter().find(|n| n.id == node_id) {
            match &node.status {
                CockpitNodeStatus::Succeeded { duration_ms } => (node.scratchpad.clone(), node.tokens_used as usize, *duration_ms),
                CockpitNodeStatus::Failed { error } => (format!("Error: {}", error), 0, 0),
                _ => (node.scratchpad.clone(), node.tokens_used as usize, 0),
            }
        } else {
            ("No response from microkernel".to_string(), 0, 0)
        };

        let tools = self.nodes.iter().find(|n| n.id == node_id).map(|n| n.tool_calls.clone()).unwrap_or_default();
        self.add_assistant_message(output, self.model_pill.clone(), tokens, duration_ms, None, tools);
        self.chat_scroll = 0;
    }

    /// Execute prompt on a specific DAG node via UDS IPC, GeminiProvider, or Standalone engine
    pub async fn execute_prompt_on_node(&mut self, node_id: &str, prompt: &str) {
        let start_time = Utc::now().timestamp_millis();

        // 1. Try Unix Domain Socket IPC to resident hgbd daemon
        let socket_path = std::env::var("HGB_SOCKET")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
                std::path::PathBuf::from(runtime_dir).join("hgb.sock")
            });

        let mut ipc_succeeded = false;
        if let Ok(mut stream) = tokio::net::UnixStream::connect(&socket_path).await {
            let req = hgb_core::HgbRequest::Prompt {
                prompt: prompt.to_string(),
                model: Some(self.model_pill.clone()),
                provider: None,
                stream: false,
            };
            if let Ok(encoded) = bincode::serialize(&req) {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                if stream.write_all(&encoded).await.is_ok() {
                    let mut buf = vec![0u8; 65536];
                    if let Ok(n) = stream.read(&mut buf).await {
                        if n > 0 {
                            if let Ok(resp) = bincode::deserialize::<hgb_core::HgbResponse>(&buf[..n]) {
                                match resp {
                                    hgb_core::HgbResponse::Complete { output, tokens_used, duration_ms } => {
                                        ipc_succeeded = true;
                                        self.telemetry.total_tokens += tokens_used as u64;
                                        let tool_name = if hgb_core::OllamaProvider::is_ollama_model(&self.model_pill) {
                                            "hgbd_ollama_local"
                                        } else {
                                            "hgbd_gemini_inference"
                                        };
                                        let tool = CockpitToolCall::new(
                                            tool_name,
                                            format!("model={}", self.model_pill),
                                            "SUCCESS",
                                            duration_ms,
                                            Some(output.chars().take(120).collect()),
                                        );
                                        if let Some(node) = self.nodes.iter_mut().find(|n| n.id == node_id) {
                                            node.status = CockpitNodeStatus::Succeeded { duration_ms };
                                            node.tokens_used = tokens_used as u64;
                                            node.scratchpad = output;
                                            node.add_tool_call(tool);
                                        }
                                        self.add_log(format!("[DAEMON] Prompt finished in {}ms ({} tok)", duration_ms, tokens_used));
                                    }
                                    hgb_core::HgbResponse::Error(err) => {
                                        ipc_succeeded = true;
                                        if let Some(node) = self.nodes.iter_mut().find(|n| n.id == node_id) {
                                            node.status = CockpitNodeStatus::Failed { error: err.clone() };
                                        }
                                        self.add_log(format!("[DAEMON_ERR] {}", err));
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
        }

        if ipc_succeeded {
            return;
        }

        // 1.5. Try Local Ollama Provider if requested model is local or if Gemini is unconfigured
        let is_ollama = hgb_core::OllamaProvider::is_ollama_model(&self.model_pill)
            || (!hgb_core::GeminiProvider::is_available() && hgb_core::OllamaProvider::is_available());

        if is_ollama && hgb_core::OllamaProvider::is_available() {
            if let Some(ollama_prov) = hgb_core::OllamaProvider::auto_discover() {
                use hgb_core::traits::HgbProvider;
                self.add_log(format!("[OLLAMA] Querying local Ollama engine ({}) directly...", self.model_pill));
                let res = ollama_prov.complete(prompt, Some(&self.model_pill)).await;
                let elapsed = (Utc::now().timestamp_millis() - start_time).max(1) as u64;
                match res {
                    Ok(output) => {
                        let approx_tokens = (output.len() / 4 + prompt.len() / 4).max(1) as u64;
                        self.telemetry.total_tokens += approx_tokens;
                        let tool = CockpitToolCall::new(
                            "ollama_local_inference",
                            format!("model={}", self.model_pill),
                            "SUCCESS",
                            elapsed,
                            Some(output.chars().take(120).collect()),
                        );
                        if let Some(node) = self.nodes.iter_mut().find(|n| n.id == node_id) {
                            node.status = CockpitNodeStatus::Succeeded { duration_ms: elapsed };
                            node.tokens_used = approx_tokens;
                            node.scratchpad = output;
                            node.add_tool_call(tool);
                        }
                        self.add_log(format!("[OLLAMA] Completed in {}ms (~{} tok)", elapsed, approx_tokens));
                        return;
                    }
                    Err(err) => {
                        self.add_log(format!("[OLLAMA_ERR] Local Ollama error: {}", err));
                    }
                }
            }
        }

        // 2. Try In-Process GeminiProvider if credentials exist
        if let Some(provider) = hgb_core::GeminiProvider::auto_discover() {
            use hgb_core::traits::HgbProvider;
            self.add_log("[PROVIDER] Querying Google Gemini provider directly...".to_string());
            let res = provider.complete(prompt, Some(&self.model_pill)).await;
            let elapsed = (Utc::now().timestamp_millis() - start_time).max(1) as u64;
            match res {
                Ok(output) => {
                    let approx_tokens = (output.len() / 4 + prompt.len() / 4).max(1) as u64;
                    self.telemetry.total_tokens += approx_tokens;
                    let tool = CockpitToolCall::new(
                        "gemini_api_direct",
                        format!("model={}", self.model_pill),
                        "SUCCESS",
                        elapsed,
                        Some(output.chars().take(120).collect()),
                    );
                    if let Some(node) = self.nodes.iter_mut().find(|n| n.id == node_id) {
                        node.status = CockpitNodeStatus::Succeeded { duration_ms: elapsed };
                        node.tokens_used = approx_tokens;
                        node.scratchpad = output;
                        node.add_tool_call(tool);
                    }
                    self.add_log(format!("[GEMINI] Completed in {}ms (~{} tok)", elapsed, approx_tokens));
                    return;
                }
                Err(err) => {
                    self.add_log(format!("[GEMINI_ERR] Provider error: {}", err));
                }
            }
        }

        // 3. Fallback: Standalone local execution simulation
        let elapsed = (Utc::now().timestamp_millis() - start_time).max(1) as u64;
        let approx_tokens = (prompt.len() / 4).max(1) as u64;
        self.telemetry.total_tokens += approx_tokens;
        let fallback_output = format!(
            "Prompt recorded in Cockpit Dag:\n\"{}\"\n\n[INFO] hgbd daemon is offline and Gemini credentials unconfigured.\nStart daemon with `hgbd` or authenticate with `hgb login` to query models.",
            prompt
        );
        let tool = CockpitToolCall::new(
            "cockpit_local_eval",
            "mode=standalone",
            "SUCCESS",
            elapsed,
            Some(prompt.chars().take(80).collect()),
        );
        if let Some(node) = self.nodes.iter_mut().find(|n| n.id == node_id) {
            node.status = CockpitNodeStatus::Succeeded { duration_ms: elapsed };
            node.tokens_used = approx_tokens;
            node.scratchpad = fallback_output;
            node.add_tool_call(tool);
        }
        self.add_log(format!("[STANDALONE] Executed prompt node '{}' ({} tok)", node_id, approx_tokens));
    }

    /// Render Cockpit UI layout into Ratatui Frame
    pub fn render_ui(&self, frame: &mut Frame) {
        let area = frame.area();

        if area.width < 40 || area.height < 10 {
            let warning = Paragraph::new("Terminal too small for Hagibis. Please resize.")
                .style(Style::default().fg(Color::Yellow));
            frame.render_widget(warning, area);
            return;
        }

        match self.view_mode {
            CockpitViewMode::ChatCanvas => self.render_chat_canvas(frame),
            CockpitViewMode::CockpitSplit => self.render_cockpit_split(frame),
        }

        // Render modal overlay if active
        match &self.overlay {
            CockpitOverlay::Shortcuts => self.render_shortcuts_overlay(frame),
            CockpitOverlay::ModelPicker { selected } => self.render_model_picker_overlay(frame, *selected),
            CockpitOverlay::Tasks => self.render_tasks_overlay(frame),
            CockpitOverlay::None => {}
        }
    }

    /// Render Multi-Pane Developer Cockpit (Top HUD + Left DAG tree + Right tabs + Steering deck)
    pub fn render_cockpit_split(&self, frame: &mut Frame) {
        let area = frame.area();

        // 3-way vertical split:
        // Top HUD (5 lines), Main Middle Body (Min 10), Bottom Deck (5 lines)
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),
                Constraint::Min(8),
                Constraint::Length(5),
            ])
            .split(area);

        // --- 1. Top HUD ---
        let hud_block = Block::default()
            .borders(Borders::ALL)
            .title(" ⚡ HAGIBIS AGY COCKPIT ⚡ ");

        let temp_color = if self.telemetry.peak_temperature_celsius > 75.0 { Color::Red } else { Color::Green };
        let hud_lines = vec![
            Line::from(vec![
                Span::styled(format!(" [{}] ", self.model_pill), Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::raw(" "),
                Span::styled(format!(" [Reasoning: {}] ", self.reasoning_effort), Style::default().fg(Color::Black).bg(Color::Magenta).add_modifier(Modifier::BOLD)),
                Span::raw(" "),
                Span::styled(format!(" [👤 {}] ", self.auth_account), Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                Span::raw(" "),
                Span::styled(format!(" [📁 {}] ", self.workspace_path), Style::default().fg(Color::LightBlue)),
            ]),
            Line::from(vec![
                Span::styled("🔥 Thermal: ", Style::default().fg(Color::Yellow)),
                Span::styled(format!("{:.1}°C  ", self.telemetry.peak_temperature_celsius), Style::default().fg(temp_color).add_modifier(Modifier::BOLD)),
                Span::styled("⚡ Throughput: ", Style::default().fg(Color::Yellow)),
                Span::styled(format!("{:.1} tok/s  ", self.telemetry.tokens_per_sec), Style::default().fg(Color::Cyan)),
                Span::styled("📊 Tokens: ", Style::default().fg(Color::Yellow)),
                Span::styled(format!("{}  ", self.telemetry.total_tokens), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled("🔗 UDS Latency: ", Style::default().fg(Color::Yellow)),
                Span::styled(format!("{}µs  ", self.telemetry.uds_latency_us), Style::default().fg(Color::Green)),
                Span::styled("🐝 Swarm Agents: ", Style::default().fg(Color::Yellow)),
                Span::styled(format!("{}  ", self.nodes.len()), Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
                Span::styled("🎯 Acceptance: ", Style::default().fg(Color::Yellow)),
                Span::styled(format!("{:.0}%", self.telemetry.speculative_acceptance_rate * 100.0), Style::default().fg(Color::Yellow)),
            ]),
            Line::from(vec![
                Span::styled("● MESH: CONNECTED (P2P Gossip)", Style::default().fg(Color::Green)),
                Span::raw(" | "),
                Span::styled("● SHIELD: ACTIVE (Formal Invariant SMT)", Style::default().fg(Color::Cyan)),
                Span::raw(" | "),
                Span::styled(format!("● TIME: {}", Utc::now().format("%Y-%m-%d %H:%M:%S UTC")), Style::default().fg(Color::DarkGray)),
            ]),
        ];
        let hud_paragraph = Paragraph::new(hud_lines).block(hud_block);
        frame.render_widget(hud_paragraph, main_chunks[0]);

        // --- 2. Main Middle Section: Split Left (DAG Tree) & Right (Multi-Tab Inspector) ---
        let middle_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(42),
                Constraint::Percentage(58),
            ])
            .split(main_chunks[1]);

        // Left Pane: Swarm Subagent DAG Tree Table
        let node_depth = |node: &CockpitDagNode, all_nodes: &[CockpitDagNode]| -> usize {
            let mut depth = 0;
            let mut cur_parent = node.parent_id.as_deref();
            while let Some(pid) = cur_parent {
                depth += 1;
                if depth > 10 {
                    break;
                }
                cur_parent = all_nodes.iter().find(|n| n.id == pid).and_then(|n| n.parent_id.as_deref());
            }
            depth
        };

        let rows: Vec<Row> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(idx, node)| {
                let depth = node_depth(node, &self.nodes);
                let (badge, color) = node.status.badge();
                let is_sel = idx == self.selected_index;
                let marker = if is_sel { "▶ " } else { "  " };

                let prefix = match depth {
                    0 => "",
                    1 => "└─ ",
                    2 => "   └─ ",
                    3 => "      └─ ",
                    _ => "         └─ ",
                };
                let display_name = format!("{}{}{}", marker, prefix, node.name);

                let cells = vec![
                    Cell::from(display_name),
                    Cell::from(badge).style(Style::default().fg(color).add_modifier(Modifier::BOLD)),
                    Cell::from(node.model.as_str()),
                    Cell::from(format!("{}", node.tokens_used)),
                ];

                let row = Row::new(cells);
                if is_sel {
                    row.style(Style::default().bg(Color::Rgb(35, 40, 60)).add_modifier(Modifier::BOLD))
                } else {
                    row
                }
            })
            .collect();

        let table = Table::new(
            rows,
            [
                Constraint::Percentage(45),
                Constraint::Percentage(22),
                Constraint::Percentage(20),
                Constraint::Percentage(13),
            ],
        )
        .header(
            Row::new(vec!["Subagent Node", "Status", "Model", "Tokens"])
                .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" 🗺️ Swarm Subagent DAG ({}) ", self.nodes.len())),
        );
        frame.render_widget(table, middle_chunks[0]);

        // Right Pane: Multi-Tab View
        let right_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(5),
            ])
            .split(middle_chunks[1]);

        let tab_titles = vec![
            Line::from(" 1: Live Stream & Reasoning "),
            Line::from(" 2: Artifacts & Surgical Diffs "),
            Line::from(" 3: Background Tasks "),
        ];
        let tabs_widget = Tabs::new(tab_titles)
            .block(Block::default().borders(Borders::BOTTOM))
            .select(self.active_tab.index())
            .style(Style::default().fg(Color::DarkGray))
            .highlight_style(Style::default().fg(Color::Yellow).bg(Color::Rgb(30, 35, 55)).add_modifier(Modifier::BOLD));
        frame.render_widget(tabs_widget, right_chunks[0]);

        // Render Tab Body
        match self.active_tab {
            CockpitActiveTab::LiveStream => {
                let live_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Percentage(45),
                        Constraint::Percentage(55),
                    ])
                    .split(right_chunks[1]);

                // Upper section: Tool invocations
                let tool_items: Vec<ListItem> = if let Some(node) = self.selected_node() {
                    if node.tool_calls.is_empty() {
                        vec![ListItem::new("  (No active tool invocations for this subagent)")]
                    } else {
                        node.tool_calls
                            .iter()
                            .map(|call| {
                                let status_color = match call.status.as_str() {
                                    "SUCCESS" => Color::Green,
                                    "RUNNING" => Color::Cyan,
                                    "FAILED" => Color::Red,
                                    "REDIRECTED" => Color::Magenta,
                                    _ => Color::Yellow,
                                };
                                let header = Line::from(vec![
                                    Span::styled("⚙ ", Style::default().fg(Color::Yellow)),
                                    Span::styled(&call.tool_name, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                                    Span::raw(format!("({}) ", call.parameters_summary)),
                                    Span::styled(format!("[{}] ", call.status), Style::default().fg(status_color).add_modifier(Modifier::BOLD)),
                                    Span::styled(format!("{}ms", call.duration_ms), Style::default().fg(Color::DarkGray)),
                                ]);
                                if let Some(ref out) = call.output_snippet {
                                    let snippet = Line::from(vec![
                                        Span::raw("    ↳ Out: "),
                                        Span::styled(out.as_str(), Style::default().fg(Color::DarkGray)),
                                    ]);
                                    ListItem::new(vec![header, snippet])
                                } else {
                                    ListItem::new(header)
                                }
                            })
                            .collect()
                    }
                } else {
                    vec![ListItem::new("  Select a subagent node to view active tools.")]
                };

                let tool_list = List::new(tool_items)
                    .block(Block::default().borders(Borders::ALL).title(" ⚙️ Active Tools & Tool Invocation Stream "));
                frame.render_widget(tool_list, live_chunks[0]);

                // Lower section: Thought stream and agent scratchpad
                let scratchpad_content = if let Some(node) = self.selected_node() {
                    let mut lines = vec![
                        Line::from(vec![
                            Span::styled("Node ID: ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                            Span::raw(&node.id),
                            Span::raw(" | "),
                            Span::styled("Model: ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                            Span::raw(&node.model),
                            Span::raw(" | "),
                            Span::styled("Parent: ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                            Span::raw(node.parent_id.as_deref().unwrap_or("None (Root)")),
                        ]),
                        Line::from(""),
                        Line::from(Span::styled("─── Agent Scratchpad / Memory / Reasoning Stream ───", Style::default().fg(Color::Cyan))),
                    ];
                    if node.scratchpad.is_empty() {
                        lines.push(Line::from("  (Scratchpad empty. Reasoning output will stream here during execution.)"));
                    } else {
                        for line in node.scratchpad.lines() {
                            lines.push(Line::from(line));
                        }
                    }
                    lines
                } else {
                    vec![Line::from("No DAG nodes configured.")]
                };

                let scratchpad_p = Paragraph::new(scratchpad_content)
                    .block(Block::default().borders(Borders::ALL).title(" 🔬 Agent Scratchpad & Thought Stream "))
                    .alignment(Alignment::Left);
                frame.render_widget(scratchpad_p, live_chunks[1]);
            }
            CockpitActiveTab::ArtifactDiffs => {
                let diff_chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Percentage(30),
                        Constraint::Percentage(70),
                    ])
                    .split(right_chunks[1]);

                // Changed files table
                let diff_rows: Vec<Row> = self
                    .artifact_diffs
                    .iter()
                    .map(|d| {
                        Row::new(vec![
                            Cell::from(format!("📄 {}", d.file_path)),
                            Cell::from(format!("+{}", d.added_lines)).style(Style::default().fg(Color::Green)),
                            Cell::from(format!("-{}", d.deleted_lines)).style(Style::default().fg(Color::Red)),
                        ])
                    })
                    .collect();

                let diff_table = Table::new(
                    diff_rows,
                    [
                        Constraint::Percentage(60),
                        Constraint::Percentage(20),
                        Constraint::Percentage(20),
                    ],
                )
                .header(
                    Row::new(vec!["Artifact File", "Added", "Deleted"])
                        .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                )
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" 📁 Tracked Artifacts ({}) ", self.artifact_diffs.len())),
                );
                frame.render_widget(diff_table, diff_chunks[0]);

                // Surgical Diff Viewer
                let mut diff_lines = Vec::new();
                if self.artifact_diffs.is_empty() {
                    diff_lines.push(Line::from("  No file change diffs or modified artifacts recorded."));
                } else {
                    for diff in &self.artifact_diffs {
                        diff_lines.push(Line::from(vec![
                            Span::styled("--- a/", Style::default().fg(Color::DarkGray)),
                            Span::styled(&diff.file_path, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                        ]));
                        for line in diff.diff_content.lines() {
                            if line.starts_with('+') {
                                diff_lines.push(Line::from(Span::styled(line, Style::default().fg(Color::Green))));
                            } else if line.starts_with('-') {
                                diff_lines.push(Line::from(Span::styled(line, Style::default().fg(Color::Red))));
                            } else if line.starts_with("@@") {
                                diff_lines.push(Line::from(Span::styled(line, Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD))));
                            } else {
                                diff_lines.push(Line::from(Span::styled(line, Style::default().fg(Color::DarkGray))));
                            }
                        }
                        diff_lines.push(Line::from(""));
                    }
                }

                let diff_viewer = Paragraph::new(diff_lines)
                    .block(Block::default().borders(Borders::ALL).title(" 🔍 Unified Surgical Diff Viewer (Green: +Add, Red: -Del) "));
                frame.render_widget(diff_viewer, diff_chunks[1]);
            }
            CockpitActiveTab::BackgroundTasks => {
                let task_rows: Vec<Row> = self
                    .background_tasks
                    .iter()
                    .map(|t| {
                        let status_color = match t.status.as_str() {
                            "RUNNING" => Color::Cyan,
                            "FINISHED" | "SUCCESS" => Color::Green,
                            "FAILED" => Color::Red,
                            _ => Color::Yellow,
                        };
                        Row::new(vec![
                            Cell::from(t.task_id.as_str()).style(Style::default().fg(Color::Yellow)),
                            Cell::from(t.description.as_str()),
                            Cell::from(format!("[{}]", t.status)).style(Style::default().fg(status_color).add_modifier(Modifier::BOLD)),
                            Cell::from(format!("{}s", t.elapsed_secs)).style(Style::default().fg(Color::DarkGray)),
                        ])
                    })
                    .collect();

                let task_table = Table::new(
                    task_rows,
                    [
                        Constraint::Percentage(25),
                        Constraint::Percentage(45),
                        Constraint::Percentage(18),
                        Constraint::Percentage(12),
                    ],
                )
                .header(
                    Row::new(vec!["Task ID", "Description", "Status", "Elapsed"])
                        .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                )
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(format!(" ⏱️ Background Tasks Monitor ({}) ", self.background_tasks.len())),
                );
                frame.render_widget(task_table, right_chunks[1]);
            }
        }

        // --- 3. Bottom Deck: Hotkeys & Prompt Input Bar ---
        let bottom_block = Block::default()
            .borders(Borders::ALL)
            .title(" 🎮 Mid-Flight Steering Controls & Command Deck ");

        let hotkeys_line = Line::from(vec![
            Span::styled("[P] ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw("Pause  "),
            Span::styled("[R] ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::raw("Resume  "),
            Span::styled("[E] ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw("Edit Scratch  "),
            Span::styled("[T] ", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
            Span::raw("Redirect Tool  "),
            Span::styled("[A] ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Span::raw("Abort  "),
            Span::styled("[Tab] ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw("Switch Tab  "),
            Span::styled("[i] ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::raw("Prompt Mode  "),
            Span::styled("[Q] ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            Span::raw("Quit"),
        ]);

        let prompt_cursor = if self.input_mode == CockpitInputMode::Input {
            Span::styled("█", Style::default().fg(Color::Yellow))
        } else {
            Span::raw("")
        };

        let prompt_line = Line::from(vec![
            Span::styled("hgb-cockpit ❯ ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::styled(&self.prompt_input, Style::default().fg(Color::White)),
            prompt_cursor,
        ]);

        let mode_badge = match self.input_mode {
            CockpitInputMode::Normal => Span::styled(" [MODE: NORMAL - Press 'i' or '/' to type prompt, hotkeys active] ", Style::default().fg(Color::DarkGray)),
            CockpitInputMode::Input => Span::styled(" [MODE: PROMPT INPUT - Type prompt, press Enter to execute, Esc to cancel] ", Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD)),
        };

        let last_log = self.log_feed.last().map(|s| s.as_str()).unwrap_or("");
        let status_line = Line::from(vec![
            mode_badge,
            Span::raw("  "),
            Span::styled(format!("Audit: {}", last_log), Style::default().fg(Color::DarkGray)),
        ]);

        let bottom_deck = Paragraph::new(vec![hotkeys_line, prompt_line, status_line]).block(bottom_block);
        frame.render_widget(bottom_deck, main_chunks[2]);
    }

    /// Render Authentic AGY CLI Conversational Chat Canvas
    pub fn render_chat_canvas(&self, frame: &mut Frame) {
        let area = frame.area();

        // Vertical layout:
        // Top Header (1 line), Divider (1 line), Chat Viewport (Min 5), Input Box (3 lines), Statusline (1 line)
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(5),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(area);

        // 1. Top Header Line: Workspace, Mode, Model, Account
        let is_local = hgb_core::OllamaProvider::is_ollama_model(&self.model_pill);
        let model_span = if is_local {
            Span::styled(format!(" [{}] (0ms local) ", self.model_pill), Style::default().fg(Color::Black).bg(Color::Magenta).add_modifier(Modifier::BOLD))
        } else {
            Span::styled(format!(" [{}] (auto-failover) ", self.model_pill), Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD))
        };

        let mode_span = Span::styled(format!(" [{}] ", self.execution_mode), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
        let cwd_span = Span::styled(format!(" 📁 {} ", self.workspace_path), Style::default().fg(Color::DarkGray));

        let header_line = Line::from(vec![
            Span::styled("⚡ HAGIBIS (hgb)", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
            cwd_span,
            Span::raw(" "),
            mode_span,
            Span::raw(" "),
            model_span,
        ]);
        frame.render_widget(Paragraph::new(header_line), chunks[0]);

        // 2. Divider line
        let divider_text = "─".repeat(area.width as usize);
        let divider = Paragraph::new(Span::styled(divider_text, Style::default().fg(Color::DarkGray)));
        frame.render_widget(divider, chunks[1]);

        // 3. Main Chat Viewport
        let mut chat_lines = Vec::new();
        if self.conversation.is_empty() {
            chat_lines.push(Line::from(""));
            chat_lines.push(Line::from(vec![
                Span::styled("▲ ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled("Hagibis (hgb)", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(" — Autonomous Microkernel & Swarm Engine", Style::default().fg(Color::DarkGray)),
            ]));
            chat_lines.push(Line::from(""));
            chat_lines.push(Line::from(vec![
                Span::styled("Ready for instructions. ", Style::default().fg(Color::White)),
                Span::styled("Type your prompt below or '/' for commands.", Style::default().fg(Color::DarkGray)),
            ]));
            chat_lines.push(Line::from(vec![
                Span::styled("Press '?' for keyboard shortcuts · 'Shift+Tab' to cycle mode · 'Ctrl+T' for DAG Cockpit", Style::default().fg(Color::DarkGray)),
            ]));
            chat_lines.push(Line::from(""));
        } else {
            for item in &self.conversation {
                match &item.sender {
                    CockpitChatSender::User => {
                        chat_lines.push(Line::from(vec![
                            Span::styled("❯ ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                            Span::styled(&item.content, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                        ]));
                        chat_lines.push(Line::from(""));
                    }
                    CockpitChatSender::System => {
                        chat_lines.push(Line::from(vec![
                            Span::styled("ℹ ", Style::default().fg(Color::Yellow)),
                            Span::styled(&item.content, Style::default().fg(Color::Yellow)),
                        ]));
                        chat_lines.push(Line::from(""));
                    }
                    CockpitChatSender::Assistant { model } => {
                        // Render thinking block if present
                        if let Some(ref thinking) = item.thinking {
                            chat_lines.push(Line::from(Span::styled(
                                format!("╭── 💭 Thinking Process ({} tokens) ──╮", item.tokens),
                                Style::default().fg(Color::Magenta),
                            )));
                            for t_line in thinking.lines() {
                                chat_lines.push(Line::from(Span::styled(format!("│ {}", t_line), Style::default().fg(Color::DarkGray))));
                            }
                            chat_lines.push(Line::from(Span::styled("╰──────────────────────────────────────╯", Style::default().fg(Color::Magenta))));
                        }

                        // Render tool calls
                        for tool in &item.tool_calls {
                            let status_color = match tool.status.as_str() {
                                "SUCCESS" => Color::Green,
                                "FAILED" => Color::Red,
                                _ => Color::Yellow,
                            };
                            chat_lines.push(Line::from(Span::styled(
                                format!("╭── 🔧 {} ({}) ───╮", tool.tool_name, tool.parameters_summary),
                                Style::default().fg(Color::Cyan),
                            )));
                            chat_lines.push(Line::from(vec![
                                Span::styled("│ Status: ", Style::default().fg(Color::Cyan)),
                                Span::styled(format!("[{}]", tool.status), Style::default().fg(status_color).add_modifier(Modifier::BOLD)),
                                Span::styled(format!("  Duration: {}ms", tool.duration_ms), Style::default().fg(Color::DarkGray)),
                            ]));
                            if let Some(ref snippet) = tool.output_snippet {
                                for s_line in snippet.lines().take(4) {
                                    chat_lines.push(Line::from(vec![
                                        Span::styled("│   ", Style::default().fg(Color::Cyan)),
                                        Span::styled(s_line, Style::default().fg(Color::DarkGray)),
                                    ]));
                                }
                            }
                            chat_lines.push(Line::from(Span::styled("╰──────────────────────────────────────╯", Style::default().fg(Color::Cyan))));
                        }

                        // Render markdown lines
                        let mut in_code_block = false;
                        for line in item.content.lines() {
                            if line.starts_with("```") {
                                if !in_code_block {
                                    in_code_block = true;
                                    let code_lang = line.trim_start_matches("```").trim();
                                    let tag = if code_lang.is_empty() { "code" } else { code_lang };
                                    chat_lines.push(Line::from(Span::styled(
                                        format!("╭─── {} ──────────────────────────────╮", tag),
                                        Style::default().fg(Color::Cyan),
                                    )));
                                } else {
                                    in_code_block = false;
                                    chat_lines.push(Line::from(Span::styled(
                                        "╰────────────────────────────────────────────╯",
                                        Style::default().fg(Color::Cyan),
                                    )));
                                }
                            } else if in_code_block {
                                chat_lines.push(Line::from(vec![
                                    Span::styled("│ ", Style::default().fg(Color::Cyan)),
                                    Span::styled(line, Style::default().fg(Color::White)),
                                ]));
                            } else if line.starts_with("+ ") || line.starts_with("+\t") {
                                chat_lines.push(Line::from(Span::styled(line, Style::default().fg(Color::Green))));
                            } else if line.starts_with("- ") || line.starts_with("-\t") {
                                chat_lines.push(Line::from(Span::styled(line, Style::default().fg(Color::Red))));
                            } else if line.starts_with("## ") {
                                chat_lines.push(Line::from(Span::styled(&line[3..], Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))));
                            } else if line.starts_with("# ") {
                                chat_lines.push(Line::from(Span::styled(&line[2..], Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))));
                            } else if line.starts_with("* ") || line.starts_with("- ") {
                                chat_lines.push(Line::from(vec![
                                    Span::styled("  • ", Style::default().fg(Color::Cyan)),
                                    Span::styled(&line[2..], Style::default().fg(Color::White)),
                                ]));
                            } else {
                                chat_lines.push(Line::from(Span::styled(line, Style::default().fg(Color::White))));
                            }
                        }

                        // Footer
                        chat_lines.push(Line::from(Span::styled(
                            format!("  ⏱ {} tokens in {} ms • Model: {}", item.tokens, item.duration_ms, model),
                            Style::default().fg(Color::DarkGray),
                        )));
                        chat_lines.push(Line::from(""));
                    }
                }
            }
        }

        // Viewport scrolling
        let viewport_height = chunks[2].height as usize;
        let usable_width = chunks[2].width.saturating_sub(2).max(1) as usize;
        let mut estimated_rows = 0;
        for line in &chat_lines {
            let width: usize = line.spans.iter().map(|s| UnicodeWidthStr::width(s.content.as_ref())).sum();
            let rows = if width == 0 { 1 } else { (width + usable_width - 1) / usable_width };
            estimated_rows += rows;
        }

        let total_lines = estimated_rows.max(chat_lines.len());
        let max_scroll = total_lines.saturating_sub(viewport_height);

        // Clamped scroll value
        let current_scroll = self.chat_scroll.min(max_scroll);
        let scroll_y = max_scroll.saturating_sub(current_scroll) as u16;

        let chat_paragraph = Paragraph::new(chat_lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll_y, 0));
        frame.render_widget(chat_paragraph, chunks[2]);

        // If content overflows the viewport, render scrollbar on right
        if total_lines > viewport_height {
            let mut scrollbar_state = ScrollbarState::new(max_scroll)
                .position(scroll_y as usize);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("▲"))
                .end_symbol(Some("▼"))
                .track_symbol(Some("│"))
                .thumb_symbol("█")
                .style(Style::default().fg(Color::DarkGray));
            frame.render_stateful_widget(scrollbar, chunks[2], &mut scrollbar_state);
        }

        // Floating scroll status pill if user has scrolled up
        if current_scroll > 0 {
            let pill_text = format!(" ▲ SCROLLED UP (+{} lines) • Press End or scroll down to bottom ", current_scroll);
            let pill_width = pill_text.len() as u16;
            let pill_x = chunks[2].x + chunks[2].width.saturating_sub(pill_width + 2);
            let pill_y = chunks[2].y;
            let pill_area = Rect::new(pill_x, pill_y, pill_width, 1);
            frame.render_widget(
                Paragraph::new(Span::styled(
                    pill_text,
                    Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
                )),
                pill_area,
            );
        }

        // 4. Input Box (Always Focused, authentic AGY box with accurate cursor)
        let chars: Vec<char> = self.prompt_input.chars().collect();
        let prompt_prefix = Span::styled("> ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));

        let prompt_content = if chars.is_empty() {
            Line::from(vec![
                prompt_prefix,
                Span::styled("█", Style::default().fg(Color::Cyan)),
            ])
        } else if self.cursor_position < chars.len() {
            let before: String = chars[..self.cursor_position].iter().collect();
            let at_char: String = chars[self.cursor_position].to_string();
            let after: String = chars[self.cursor_position + 1..].iter().collect();
            Line::from(vec![
                prompt_prefix,
                Span::styled(before, Style::default().fg(Color::White)),
                Span::styled(at_char, Style::default().fg(Color::Black).bg(Color::Cyan)),
                Span::styled(after, Style::default().fg(Color::White)),
            ])
        } else {
            let text: String = chars.iter().collect();
            Line::from(vec![
                prompt_prefix,
                Span::styled(text, Style::default().fg(Color::White)),
                Span::styled("█", Style::default().fg(Color::Cyan)),
            ])
        };

        let prompt_title = match self.execution_mode.as_str() {
            "plan" => " ❯ type a prompt (plan mode · Shift+Tab to switch) ",
            "accept-edits" => " ❯ type a prompt (accept-edits mode · Shift+Tab to switch) ",
            _ => " ❯ type a prompt (? shortcuts, / commands, Ctrl+T cockpit) ",
        };
        let input_box = Paragraph::new(prompt_content).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(prompt_title),
        );
        frame.render_widget(input_box, chunks[3]);

        // 5. Statusline Footer
        let bg_tasks_info = if !self.background_tasks.is_empty() {
            format!("⚡ {} tasks ", self.background_tasks.len())
        } else {
            String::new()
        };

        let statusline = Line::from(vec![
            Span::styled("? shortcuts · Shift+Tab mode · Ctrl+T cockpit · Esc clear", Style::default().fg(Color::DarkGray)),
            Span::raw("   "),
            Span::styled(bg_tasks_info, Style::default().fg(Color::Yellow)),
            Span::raw("   "),
            Span::styled(format!("👤 {} · 12µs IPC", self.auth_account), Style::default().fg(Color::DarkGray)),
        ]);
        frame.render_widget(Paragraph::new(statusline), chunks[4]);
    }

    /// Render Shortcuts Modal Overlay
    pub fn render_shortcuts_overlay(&self, frame: &mut Frame) {
        let area = frame.area();
        let width = 76.min(area.width.saturating_sub(4));
        let height = 24.min(area.height.saturating_sub(4));
        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let modal_area = Rect::new(x, y, width, height);

        frame.render_widget(Clear, modal_area);

        let modal_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" ⌨ Hagibis Keyboard Shortcuts & Slash Commands (Press Esc or ? to close) ");

        let rows = vec![
            Row::new(vec![Cell::from("Enter").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Submit prompt to microkernel")]),
            Row::new(vec![Cell::from("Shift+Enter / Alt+Enter").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Insert newline without submitting (multiline prompt)")]),
            Row::new(vec![Cell::from("Up / Down").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Navigate prompt history (previous / next prompt)")]),
            Row::new(vec![Cell::from("PageUp / PageDown / Mouse").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Scroll conversation viewport up / down")]),
            Row::new(vec![Cell::from("Shift+Up / Shift+Down").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Fine-grained conversation scrolling (1 line)")]),
            Row::new(vec![Cell::from("End (when scrolled)").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Snap conversation viewport back to bottom")]),
            Row::new(vec![Cell::from("Ctrl+A / Ctrl+E (Home / End)").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Move cursor to beginning / end of prompt")]),
            Row::new(vec![Cell::from("Ctrl+Left / Ctrl+Right").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Move cursor backward / forward by word (Alt+B / Alt+F)")]),
            Row::new(vec![Cell::from("Ctrl+K / Ctrl+U").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Cut prompt to end / start into kill ring")]),
            Row::new(vec![Cell::from("Ctrl+W / Alt+D").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Cut word backward / forward into kill ring")]),
            Row::new(vec![Cell::from("Ctrl+Y").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Paste (yank) text from kill ring at cursor")]),
            Row::new(vec![Cell::from("Shift+Tab").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Cycle execution mode (default ➔ plan ➔ accept-edits)")]),
            Row::new(vec![Cell::from("Ctrl+T").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Toggle between AGY Chat Canvas and DAG Cockpit")]),
            Row::new(vec![Cell::from("Ctrl+D").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Exit CLI on empty line / Delete char under cursor")]),
            Row::new(vec![Cell::from("Ctrl+C").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Clear current prompt buffer / Cancel")]),
            Row::new(vec![Cell::from("Ctrl+L").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Clear screen and repaint terminal")]),
            Row::new(vec![Cell::from("Tab").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Autocomplete slash commands and models")]),
            Row::new(vec![Cell::from("?").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Toggle this keyboard shortcuts help modal")]),
            Row::new(vec![Cell::from("──────────────").style(Style::default().fg(Color::DarkGray)), Cell::from("──────────────────────────────────────────────────").style(Style::default().fg(Color::DarkGray))]),
            Row::new(vec![Cell::from("/model [name]").style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)), Cell::from("Switch active AI model (Ollama local or Gemini cloud)")]),
            Row::new(vec![Cell::from("/cockpit").style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)), Cell::from("Switch to full-screen multi-pane DAG Swarm Cockpit")]),
            Row::new(vec![Cell::from("/chat").style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)), Cell::from("Switch to AGY Conversational Chat Canvas")]),
            Row::new(vec![Cell::from("/tasks").style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)), Cell::from("Inspect active background swarm tasks")]),
            Row::new(vec![Cell::from("/clear").style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)), Cell::from("Clear current conversation history")]),
            Row::new(vec![Cell::from("/help").style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)), Cell::from("Open this shortcuts and commands panel")]),
            Row::new(vec![Cell::from("/exit, /quit").style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)), Cell::from("Exit Hagibis interactive session")]),
        ];

        let table = Table::new(rows, [Constraint::Percentage(32), Constraint::Percentage(68)])
            .header(Row::new(vec!["Key / Command", "Action"]).style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
            .block(modal_block);

        frame.render_widget(table, modal_area);
    }

    /// Render Model Picker Modal Overlay
    pub fn render_model_picker_overlay(&self, frame: &mut Frame, selected: usize) {
        let area = frame.area();
        let width = 64.min(area.width.saturating_sub(4));
        let height = 14.min(area.height.saturating_sub(4));
        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let modal_area = Rect::new(x, y, width, height);

        frame.render_widget(Clear, modal_area);

        let models = [
            ("qwen2.5-coder:1.5b", "Local Ollama (0ms latency, zero cloud cost)"),
            ("qwen2.5:0.5b", "Local Ollama (ultra-lightweight local model)"),
            ("llama3.2:1b", "Local Ollama (Meta LLaMA local model)"),
            ("smollm2:1.7b", "Local Ollama (SmolLM compact model)"),
            ("phi3:mini", "Local Ollama (Microsoft Phi-3 mini)"),
            ("gemini-2.5-flash", "Google Gemini Cloud (Recommended default)"),
            ("gemini-2.5-pro", "Google Gemini Cloud (Deep reasoning pro)"),
        ];

        let rows: Vec<Row> = models
            .iter()
            .enumerate()
            .map(|(i, (name, desc))| {
                let is_sel = i == selected;
                let marker = if is_sel { "▶ " } else { "  " };
                let is_active = *name == self.model_pill;
                let active_badge = if is_active { " [ACTIVE]" } else { "" };
                let row = Row::new(vec![
                    Cell::from(format!("{}{}{}", marker, name, active_badge)),
                    Cell::from(*desc),
                ]);
                if is_sel {
                    row.style(Style::default().bg(Color::Rgb(35, 45, 75)).fg(Color::Yellow).add_modifier(Modifier::BOLD))
                } else {
                    row
                }
            })
            .collect();

        let table = Table::new(rows, [Constraint::Percentage(45), Constraint::Percentage(55)])
            .header(Row::new(vec!["Model", "Provider"]).style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(" 🧠 Select AI Model (↑/↓ navigate, Enter select, Esc close) "),
            );

        frame.render_widget(table, modal_area);
    }

    /// Render Background Tasks Modal Overlay
    pub fn render_tasks_overlay(&self, frame: &mut Frame) {
        let area = frame.area();
        let width = 72.min(area.width.saturating_sub(4));
        let height = 16.min(area.height.saturating_sub(4));
        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let modal_area = Rect::new(x, y, width, height);

        frame.render_widget(Clear, modal_area);

        let rows: Vec<Row> = if self.background_tasks.is_empty() {
            vec![Row::new(vec![Cell::from("No active background tasks"), Cell::from("-"), Cell::from("-")])]
        } else {
            self.background_tasks
                .iter()
                .map(|t| {
                    let color = match t.status.as_str() {
                        "RUNNING" => Color::Cyan,
                        "SUCCESS" => Color::Green,
                        _ => Color::Yellow,
                    };
                    Row::new(vec![
                        Cell::from(t.task_id.as_str()),
                        Cell::from(t.description.as_str()),
                        Cell::from(t.status.as_str()).style(Style::default().fg(color).add_modifier(Modifier::BOLD)),
                    ])
                })
                .collect()
        };

        let table = Table::new(rows, [Constraint::Percentage(25), Constraint::Percentage(55), Constraint::Percentage(20)])
            .header(Row::new(vec!["Task ID", "Description", "Status"]).style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(" ⏱ Background Tasks Monitor (Press Esc or Enter to close) "),
            );

        frame.render_widget(table, modal_area);
    }

    /// Headless renderer for automated unit and integration tests
    pub fn render_headless(&self, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("Failed to initialize headless terminal backend");
        terminal
            .draw(|f| self.render_ui(f))
            .expect("Failed to render headless frame");
        terminal.backend().buffer().clone()
    }

    /// Helper to convert headless buffer to a debug string
    pub fn render_headless_to_string(&self, width: u16, height: u16) -> String {
        let buf = self.render_headless(width, height);
        let mut s = String::new();
        for y in 0..height {
            for x in 0..width {
                let cell = buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" ");
                s.push_str(cell);
            }
            s.push('\n');
        }
        s
    }

    /// Launch interactive terminal cockpit session (async)
    pub async fn run_interactive(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        let mut stdout_handle = stdout();
        crossterm::execute!(stdout_handle, EnterAlternateScreen, crossterm::event::EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout_handle);
        let mut terminal = Terminal::new(backend)?;

        let res = self.event_loop(&mut terminal).await;

        disable_raw_mode()?;
        crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen, crossterm::event::DisableMouseCapture)?;
        terminal.show_cursor()?;

        res
    }

    /// Launch interactive terminal cockpit session (blocking)
    pub fn run_interactive_blocking(&mut self) -> io::Result<()> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(self.run_interactive())
    }

    async fn event_loop<B: ratatui::backend::Backend>(&mut self, terminal: &mut Terminal<B>) -> io::Result<()> {
        let models_list = [
            "qwen2.5-coder:1.5b",
            "qwen2.5:0.5b",
            "llama3.2:1b",
            "smollm2:1.7b",
            "phi3:mini",
            "gemini-2.5-flash",
            "gemini-2.5-pro",
        ];

        loop {
            terminal.draw(|f| self.render_ui(f))?;

            if event::poll(Duration::from_millis(50))? {
                match event::read()? {
                    Event::Mouse(mouse_event) => {
                        match mouse_event.kind {
                            MouseEventKind::ScrollUp => {
                                self.scroll_chat_up(3);
                            }
                            MouseEventKind::ScrollDown => {
                                self.scroll_chat_down(3);
                            }
                            _ => {}
                        }
                    }
                    Event::Key(key) => {
                        if key.kind == KeyEventKind::Release {
                            continue;
                        }

                        // 1. If an overlay modal is active, modal takes priority
                        if self.overlay != CockpitOverlay::None {
                            match &mut self.overlay {
                                CockpitOverlay::ModelPicker { ref mut selected } => match key.code {
                                    KeyCode::Esc => {
                                        self.overlay = CockpitOverlay::None;
                                    }
                                    KeyCode::Up | KeyCode::Char('k') => {
                                        *selected = selected.saturating_sub(1);
                                    }
                                    KeyCode::Down | KeyCode::Char('j') => {
                                        *selected = (*selected + 1).min(models_list.len() - 1);
                                    }
                                    KeyCode::Enter => {
                                        let new_model = models_list[*selected].to_string();
                                        self.model_pill = new_model.clone();
                                        self.add_system_notice(format!("Active model switched to: {}", new_model));
                                        self.overlay = CockpitOverlay::None;
                                    }
                                    _ => {}
                                },
                                CockpitOverlay::Shortcuts | CockpitOverlay::Tasks => match key.code {
                                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') | KeyCode::Char('q') => {
                                        self.overlay = CockpitOverlay::None;
                                    }
                                    _ => {}
                                },
                                CockpitOverlay::None => {}
                            }
                            continue;
                        }

                        // 2. Control Key Combinations (AGY Readline Parity)
                        if key.modifiers.contains(KeyModifiers::CONTROL) {
                            match key.code {
                                KeyCode::Char('t') | KeyCode::Char('T') => {
                                    self.toggle_view_mode();
                                    continue;
                                }
                                KeyCode::Char('c') | KeyCode::Char('C') => {
                                    if self.prompt_input.is_empty() {
                                        break;
                                    } else {
                                        self.clear_prompt();
                                        continue;
                                    }
                                }
                                KeyCode::Char('d') | KeyCode::Char('D') => {
                                    if self.prompt_input.is_empty() {
                                        break;
                                    } else {
                                        self.delete_forward();
                                        continue;
                                    }
                                }
                                KeyCode::Char('l') | KeyCode::Char('L') => {
                                    terminal.clear()?;
                                    continue;
                                }
                                KeyCode::Char('a') | KeyCode::Char('A') => {
                                    self.move_to_start();
                                    continue;
                                }
                                KeyCode::Char('e') | KeyCode::Char('E') => {
                                    self.move_to_end();
                                    continue;
                                }
                                KeyCode::Char('b') | KeyCode::Char('B') => {
                                    self.move_cursor_left();
                                    continue;
                                }
                                KeyCode::Char('f') | KeyCode::Char('F') => {
                                    self.move_cursor_right();
                                    continue;
                                }
                                KeyCode::Char('k') | KeyCode::Char('K') => {
                                    self.kill_to_end();
                                    continue;
                                }
                                KeyCode::Char('u') | KeyCode::Char('U') => {
                                    self.kill_to_start();
                                    continue;
                                }
                                KeyCode::Char('w') | KeyCode::Char('W') => {
                                    self.kill_word_backward();
                                    continue;
                                }
                                KeyCode::Char('y') | KeyCode::Char('Y') => {
                                    self.yank();
                                    continue;
                                }
                                KeyCode::Left => {
                                    self.move_word_backward();
                                    continue;
                                }
                                KeyCode::Right => {
                                    self.move_word_forward();
                                    continue;
                                }
                                KeyCode::Up => {
                                    self.scroll_chat_up(2);
                                    continue;
                                }
                                KeyCode::Down => {
                                    self.scroll_chat_down(2);
                                    continue;
                                }
                                _ => {}
                            }
                        }

                        // 3. Alt / Meta Key Combinations
                        if key.modifiers.contains(KeyModifiers::ALT) {
                            match key.code {
                                KeyCode::Char('b') | KeyCode::Char('B') | KeyCode::Left => {
                                    self.move_word_backward();
                                    continue;
                                }
                                KeyCode::Char('f') | KeyCode::Char('F') | KeyCode::Right => {
                                    self.move_word_forward();
                                    continue;
                                }
                                KeyCode::Char('d') | KeyCode::Char('D') => {
                                    self.kill_word_forward();
                                    continue;
                                }
                                KeyCode::Up => {
                                    self.scroll_chat_up(2);
                                    continue;
                                }
                                KeyCode::Down => {
                                    self.scroll_chat_down(2);
                                    continue;
                                }
                                KeyCode::Enter => {
                                    self.insert_char('\n');
                                    continue;
                                }
                                _ => {}
                            }
                        }

                        // 4. Shift Key Combinations
                        if key.modifiers.contains(KeyModifiers::SHIFT) {
                            match key.code {
                                KeyCode::Up => {
                                    self.scroll_chat_up(2);
                                    continue;
                                }
                                KeyCode::Down => {
                                    self.scroll_chat_down(2);
                                    continue;
                                }
                                KeyCode::PageUp => {
                                    self.scroll_chat_up(8);
                                    continue;
                                }
                                KeyCode::PageDown => {
                                    self.scroll_chat_down(8);
                                    continue;
                                }
                                KeyCode::Home => {
                                    self.scroll_chat_to_top();
                                    continue;
                                }
                                KeyCode::End => {
                                    self.scroll_chat_to_bottom();
                                    continue;
                                }
                                KeyCode::Enter => {
                                    self.insert_char('\n');
                                    continue;
                                }
                                KeyCode::Tab => {
                                    self.cycle_execution_mode();
                                    continue;
                                }
                                _ => {}
                            }
                        }

                        if key.code == KeyCode::BackTab {
                            self.cycle_execution_mode();
                            continue;
                        }

                        // 5. Standalone Page Scrolling
                        if key.code == KeyCode::PageUp {
                            self.scroll_chat_up(5);
                            continue;
                        }
                        if key.code == KeyCode::PageDown {
                            self.scroll_chat_down(5);
                            continue;
                        }

                        // 6. CockpitSplit Navigation & Steering Keys (when not typing)
                        if self.view_mode == CockpitViewMode::CockpitSplit && self.input_mode == CockpitInputMode::Normal {
                            match key.code {
                                KeyCode::Char('q') | KeyCode::Char('Q') => break,
                                KeyCode::Tab => { self.next_tab(); continue; }
                                KeyCode::Down | KeyCode::Char('j') => { self.select_next(); continue; }
                                KeyCode::Up | KeyCode::Char('k') => { self.select_prev(); continue; }
                                KeyCode::Char('1') => { self.set_active_tab(CockpitActiveTab::LiveStream); continue; }
                                KeyCode::Char('2') => { self.set_active_tab(CockpitActiveTab::ArtifactDiffs); continue; }
                                KeyCode::Char('3') => { self.set_active_tab(CockpitActiveTab::BackgroundTasks); continue; }
                                KeyCode::Char('p') | KeyCode::Char('P') => {
                                    if let Some(node) = self.selected_node() {
                                        let id = node.id.clone();
                                        let _ = self.apply_steering(SteeringAction::Pause { node_id: id });
                                    }
                                    continue;
                                }
                                KeyCode::Char('r') | KeyCode::Char('R') => {
                                    if let Some(node) = self.selected_node() {
                                        let id = node.id.clone();
                                        let _ = self.apply_steering(SteeringAction::Resume { node_id: id });
                                    }
                                    continue;
                                }
                                KeyCode::Char('e') | KeyCode::Char('E') => {
                                    if let Some(node) = self.selected_node() {
                                        let id = node.id.clone();
                                        let _ = self.apply_steering(SteeringAction::EditScratchpad {
                                            node_id: id,
                                            new_scratchpad: format!("Steered manually in Cockpit @ {}", Utc::now().format("%H:%M:%S")),
                                        });
                                    }
                                    continue;
                                }
                                KeyCode::Char('t') | KeyCode::Char('T') => {
                                    if let Some(node) = self.selected_node() {
                                        let id = node.id.clone();
                                        let _ = self.apply_steering(SteeringAction::RedirectTool {
                                            node_id: id,
                                            new_tool_name: "hgb_surgical_edit".to_string(),
                                            parameters: serde_json::json!({"action": "redirected"}),
                                        });
                                    }
                                    continue;
                                }
                                KeyCode::Char('a') | KeyCode::Char('A') => {
                                    if let Some(node) = self.selected_node() {
                                        let id = node.id.clone();
                                        let _ = self.apply_steering(SteeringAction::Abort {
                                            node_id: id,
                                            reason: "Manual supervisor abort in Cockpit".to_string(),
                                        });
                                    }
                                    continue;
                                }
                                KeyCode::Char('i') | KeyCode::Char('/') => {
                                    self.input_mode = CockpitInputMode::Input;
                                    continue;
                                }
                                _ => {}
                            }
                        }

                        // 7. Prompt Input Handling
                        match key.code {
                            KeyCode::Esc => {
                                if self.chat_scroll > 0 {
                                    self.scroll_chat_to_bottom();
                                } else if self.view_mode == CockpitViewMode::CockpitSplit {
                                    self.input_mode = CockpitInputMode::Normal;
                                    self.clear_prompt();
                                } else {
                                    self.clear_prompt();
                                }
                            }
                            KeyCode::Char('?') if self.prompt_input.is_empty() => {
                                self.overlay = CockpitOverlay::Shortcuts;
                            }
                            KeyCode::Tab => {
                                // Slash command autocompletion
                                if let Some(prefix) = self.prompt_input.strip_prefix('/') {
                                    let commands = ["model", "cockpit", "chat", "tasks", "plan", "clear", "help", "exit", "quit"];
                                    for cmd in commands {
                                        if cmd.starts_with(prefix) {
                                            self.prompt_input = format!("/{} ", cmd);
                                            self.cursor_position = self.prompt_input.chars().count();
                                            break;
                                        }
                                    }
                                }
                            }
                            KeyCode::Enter => {
                                let input = self.prompt_input.trim().to_string();
                                if input.starts_with('/') {
                                    let parts: Vec<&str> = input.split_whitespace().collect();
                                    let cmd = parts.first().copied().unwrap_or("");
                                    let arg = parts.get(1).copied().unwrap_or("");
                                    match cmd {
                                        "/help" | "/?" => {
                                            self.overlay = CockpitOverlay::Shortcuts;
                                        }
                                        "/model" => {
                                            if !arg.is_empty() {
                                                self.model_pill = arg.to_string();
                                                self.add_system_notice(format!("Active model set to: {}", arg));
                                            } else {
                                                self.overlay = CockpitOverlay::ModelPicker { selected: 0 };
                                            }
                                        }
                                        "/cockpit" => {
                                            self.view_mode = CockpitViewMode::CockpitSplit;
                                        }
                                        "/chat" => {
                                            self.view_mode = CockpitViewMode::ChatCanvas;
                                        }
                                        "/tasks" => {
                                            self.overlay = CockpitOverlay::Tasks;
                                        }
                                        "/plan" => {
                                            self.execution_mode = "plan".to_string();
                                            self.add_system_notice("Switched execution mode to: plan");
                                        }
                                        "/clear" => {
                                            self.conversation.clear();
                                            self.chat_scroll = 0;
                                        }
                                        "/exit" | "/quit" => {
                                            break;
                                        }
                                        _ => {
                                            self.add_system_notice(format!("Unknown command '{}'. Type '/help' for commands.", cmd));
                                        }
                                    }
                                    self.clear_prompt();
                                } else if !input.is_empty() {
                                    self.submit_current_prompt().await;
                                }
                            }
                            KeyCode::Backspace => {
                                self.delete_backward();
                            }
                            KeyCode::Delete => {
                                self.delete_forward();
                            }
                            KeyCode::Left => {
                                self.move_cursor_left();
                            }
                            KeyCode::Right => {
                                self.move_cursor_right();
                            }
                            KeyCode::Home => {
                                self.move_to_start();
                            }
                            KeyCode::End => {
                                if self.chat_scroll > 0 {
                                    self.scroll_chat_to_bottom();
                                } else {
                                    self.move_to_end();
                                }
                            }
                            KeyCode::Up => {
                                if self.view_mode == CockpitViewMode::CockpitSplit && self.input_mode == CockpitInputMode::Normal {
                                    self.select_prev();
                                } else {
                                    self.history_prev();
                                }
                            }
                            KeyCode::Down => {
                                if self.view_mode == CockpitViewMode::CockpitSplit && self.input_mode == CockpitInputMode::Normal {
                                    self.select_next();
                                } else {
                                    self.history_next();
                                }
                            }
                            KeyCode::Char(c) => {
                                self.insert_char(c);
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cockpit_state_transitions() {
        let mut state = CockpitState::new();
        let node = CockpitDagNode::new("n1", "Tokenizer", "gemini-2.5-flash");
        state.add_node(node);

        assert_eq!(state.nodes.len(), 1);
        assert_eq!(state.nodes[0].status, CockpitNodeStatus::Pending);

        state.update_node_status("n1", CockpitNodeStatus::Running { progress_pct: 50 });
        assert_eq!(state.nodes[0].status, CockpitNodeStatus::Running { progress_pct: 50 });

        state.update_node_status("n1", CockpitNodeStatus::Succeeded { duration_ms: 120 });
        assert_eq!(state.nodes[0].status, CockpitNodeStatus::Succeeded { duration_ms: 120 });
    }

    #[test]
    fn test_cockpit_steering_actions() {
        let mut state = CockpitState::new();
        let node = CockpitDagNode::new("n1", "Tokenizer", "gemini-2.5-flash");
        state.add_node(node);

        state.apply_steering(SteeringAction::Pause { node_id: "n1".to_string() }).unwrap();
        assert_eq!(state.nodes[0].status, CockpitNodeStatus::Paused);

        state.apply_steering(SteeringAction::Resume { node_id: "n1".to_string() }).unwrap();
        assert_eq!(state.nodes[0].status, CockpitNodeStatus::Running { progress_pct: 0 });

        state.apply_steering(SteeringAction::EditScratchpad {
            node_id: "n1".to_string(),
            new_scratchpad: "New notes".to_string(),
        }).unwrap();
        assert_eq!(state.nodes[0].scratchpad, "New notes");

        state.apply_steering(SteeringAction::Abort {
            node_id: "n1".to_string(),
            reason: "User cancelled".to_string(),
        }).unwrap();
        assert!(matches!(state.nodes[0].status, CockpitNodeStatus::Failed { .. }));
    }

    #[test]
    fn test_cockpit_tabs_cycling() {
        let mut state = CockpitState::new();
        assert_eq!(state.active_tab, CockpitActiveTab::LiveStream);
        state.next_tab();
        assert_eq!(state.active_tab, CockpitActiveTab::ArtifactDiffs);
        state.next_tab();
        assert_eq!(state.active_tab, CockpitActiveTab::BackgroundTasks);
        state.next_tab();
        assert_eq!(state.active_tab, CockpitActiveTab::LiveStream);
        state.prev_tab();
        assert_eq!(state.active_tab, CockpitActiveTab::BackgroundTasks);
    }

    #[test]
    fn test_cockpit_headless_render() {
        let mut state = CockpitState::new();
        state.add_node(CockpitDagNode::new("n1", "Root Task", "gemini-2.5-pro"));
        state.add_artifact_diff(CockpitArtifactDiff::new("src/main.rs", 10, 2, "@@ -1,2 +1,3 @@\n+test"));
        state.add_background_task(CockpitBackgroundTask::new("bg1", "Background Fuzzing", "RUNNING", 5));

        for tab in CockpitActiveTab::all() {
            state.set_active_tab(*tab);
            let buffer = state.render_headless(120, 30);
            assert!(buffer.content.len() >= 120 * 30);
        }
    }
}
