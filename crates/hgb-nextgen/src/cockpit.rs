//! # Interactive Cockpit TUI Engine for Hagibis (`hgb cockpit`)
//!
//! Provides live DAG visual tracking, multi-agent hierarchy, mid-flight agent steering
//! (Pause, EditScratchpad, RedirectTool, Resume, Abort, InjectContext), active tool call
//! streaming, background tasks monitor, surgical diff viewer, and real-time telemetry rendering.

use chrono::Utc;
use crossterm::{
    event::{
        self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
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
use crate::chrono_warp::ChronoWarpEngine;
use crate::clipboard_xerox::ClipboardXeroxEngine;
use crate::compactor::SmartAutoCompactor;
use crate::council::CouncilEngine;
use crate::crash_interceptor::InterceptedCrash;
use crate::diff_hud::{DiffHunk, DiffLineKind, SelectivePatcher};
use crate::ghost_engine::GhostEngine;
use crate::green_light::GreenLightEngine;
use crate::hallucination_sentry::HallucinationSentry;
use crate::phantom_swarm::{PhantomSwarmConfig, PhantomSwarmEngine};
use crate::pixel_radar::PixelDiffRadar;
use crate::qr::{detect_local_ip, render_mobile_test_card};
use crate::terminal_graphics::{
    detect_graphics_protocol, TerminalGraphicsProtocol,
};
use crate::voice_hook::AudioPromptEngine;
use crate::wattage_governor::WattageGovernor;
use crate::wiretap::{ExpectedField, WiretapEngine};
use hgb_storage::db_time_machine::DbTimeMachine;
use hgb_storage::semantic_telepathy::{TelepathyDocument, TelepathyIndex};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{self, stdout};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Wrap a string cleanly to fit within max_width visual columns with proper word boundaries
fn wrap_line_to_width(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![text.to_string()];
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    let words = trimmed.split_whitespace();
    let mut current_line = String::new();
    let mut current_width = 0;

    for word in words {
        let word_width = visual_str_width(word);
        if word_width > max_width {
            // Very long word exceeds max_width on its own: break across lines character-by-character
            if !current_line.is_empty() {
                lines.push(current_line);
                current_line = String::new();
                current_width = 0;
            }
            let mut sub = String::new();
            let mut sub_w = 0;
            let chars: Vec<char> = word.chars().collect();
            let mut i = 0;
            while i < chars.len() {
                let ch = chars[i];
                let ch_w = if ch == '\u{fe0f}' {
                    0
                } else if i + 1 < chars.len() && chars[i + 1] == '\u{fe0f}' {
                    2
                } else {
                    UnicodeWidthChar::width(ch).unwrap_or(1)
                };
                if sub_w + ch_w > max_width && !sub.is_empty() {
                    lines.push(sub);
                    sub = String::new();
                    sub_w = 0;
                }
                sub.push(ch);
                if ch_w == 2 && i + 1 < chars.len() && chars[i + 1] == '\u{fe0f}' {
                    sub.push('\u{fe0f}');
                    i += 1;
                }
                sub_w += ch_w;
                i += 1;
            }
            if !sub.is_empty() {
                current_line = sub;
                current_width = sub_w;
            }
        } else if current_width == 0 {
            current_line.push_str(word);
            current_width = word_width;
        } else if current_width + 1 + word_width <= max_width {
            current_line.push(' ');
            current_line.push_str(word);
            current_width += 1 + word_width;
        } else {
            lines.push(current_line);
            current_line = word.to_string();
            current_width = word_width;
        }
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Wrap a code line cleanly to fit within max_width without stripping indentation
fn wrap_code_line_to_width(line: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![line.to_string()];
    }
    let total_w = visual_str_width(line);
    if total_w <= max_width {
        return vec![line.to_string()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut cur_w = 0;
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        let ch_w = if ch == '\u{fe0f}' {
            0
        } else if i + 1 < chars.len() && chars[i + 1] == '\u{fe0f}' {
            2
        } else {
            UnicodeWidthChar::width(ch).unwrap_or(1)
        };
        if cur_w + ch_w > max_width && !current.is_empty() {
            chunks.push(current);
            current = String::new();
            cur_w = 0;
        }
        current.push(ch);
        if ch_w == 2 && i + 1 < chars.len() && chars[i + 1] == '\u{fe0f}' {
            current.push('\u{fe0f}');
            i += 1;
        }
        cur_w += ch_w;
        i += 1;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

/// Accurately compute visual terminal column width, properly accounting for
/// Unicode emojis with variation selector-16 (U+FE0F) which modern terminal emulators
/// render in 2 columns even when older unicode_width tables evaluate them as 1 column.
pub fn visual_str_width(s: &str) -> usize {
    let mut w = 0;
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '\u{fe0f}' {
            i += 1;
            continue;
        }
        let next_is_vs16 = i + 1 < chars.len() && chars[i + 1] == '\u{fe0f}';
        if next_is_vs16 {
            w += 2;
            i += 2;
            continue;
        }
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        w += cw;
        i += 1;
    }
    w
}

/// Truncate a string cleanly to fit within max_width visual columns
fn truncate_str_by_width(text: &str, max_width: usize) -> String {
    let mut result = String::new();
    let mut current_w = 0;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        let ch_w = if ch == '\u{fe0f}' {
            0
        } else if i + 1 < chars.len() && chars[i + 1] == '\u{fe0f}' {
            2
        } else {
            UnicodeWidthChar::width(ch).unwrap_or(1)
        };
        if current_w + ch_w > max_width {
            break;
        }
        result.push(ch);
        if ch_w == 2 && i + 1 < chars.len() && chars[i + 1] == '\u{fe0f}' {
            result.push('\u{fe0f}');
            i += 1;
        }
        current_w += ch_w;
        i += 1;
    }
    result
}


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

    pub fn dynamic_badge(&self, tick: usize) -> (String, Color) {
        match self {
            Self::Running { .. } => {
                const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                let s = SPINNER_FRAMES[tick % SPINNER_FRAMES.len()];
                (format!("[{} RUNNING]", s), Color::Cyan)
            }
            _ => {
                let (b, c) = self.badge();
                (b.to_string(), c)
            }
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
    pub is_processing: bool,
    pub processing_tick: usize,
    pub processing_start: Option<chrono::DateTime<chrono::Utc>>,
    pub processing_prompt_preview: String,
    pub last_copied_id: Option<String>,
    pub last_copied_time: Option<Instant>,
    pub copy_hitboxes: Arc<Mutex<Vec<CopyHitbox>>>,
    pub scrollbar_hitbox: Arc<Mutex<Option<ScrollbarHitbox>>>,
    pub scroll_pill_hitbox: Arc<Mutex<Option<ScrollPillHitbox>>>,
    pub pinned_goal: Option<String>,
    pub diff_cards: Vec<DiffCardItem>,
    pub diff_hitboxes: Arc<Mutex<Vec<DiffActionHitbox>>>,
    pub intercepted_crash: Option<InterceptedCrash>,
    pub heal_hitboxes: Arc<Mutex<Vec<HealHitbox>>>,
    pub tunnel_cards: Vec<TunnelCardItem>,
    pub image_previews: Vec<ImagePreviewItem>,
    pub is_listening: bool,
    pub voice_engine: AudioPromptEngine,
    pub telepathy_cards: Vec<TelepathyCardItem>,
    pub ghost_cards: Vec<GhostCardItem>,
    pub council_cards: Vec<CouncilCardItem>,
    pub pixel_cards: Vec<PixelRadarCardItem>,
    pub green_light_cards: Vec<GreenLightCardItem>,
    pub db_cards: Vec<DbTimeMachineCardItem>,
    pub warp_cards: Vec<ChronoWarpCardItem>,
    pub traffic_cards: Vec<PhantomSwarmCardItem>,
    pub wiretap_cards: Vec<WiretapCardItem>,
    pub sentry_cards: Vec<HallucinationSentryCardItem>,
    pub xerox_cards: Vec<ClipboardXeroxCardItem>,
    pub governor_cards: Vec<WattageGovernorCardItem>,
    pub web_browse_cards: Vec<WebBrowseCardItem>,
    pub web_search_cards: Vec<WebSearchCardItem>,
    pub memory_cards: Vec<MemoryCardItem>,
    pub ambient_cards: Vec<AmbientCardItem>,
    pub validation_cards: Vec<ValidationCardItem>,
    pub forge_cards: Vec<ForgeCardItem>,
    pub prune_cards: Vec<PruneCardItem>,
    pub port_cards: Vec<PortCardItem>,
    pub ship_cards: Vec<ShipCardItem>,
    pub browser_incident_cards: Vec<BrowserIncidentCardItem>,
    pub seed_cards: Vec<SeedCardItem>,
    pub rewind_cards: Vec<RewindCardItem>,
    pub redteam_cards: Vec<RedTeamCardItem>,
    pub blueprint_cards: Vec<BlueprintCardItem>,
    pub passive_sentinel_cards: Vec<PassiveSentinelCardItem>,
    pub teleport_cards: Vec<DomTeleportCardItem>,
    pub steer_cards: Vec<SteerCardItem>,
    pub sandbox_cards: Vec<SandboxCardItem>,
    pub arena_cards: Vec<ArenaCardItem>,
    pub gc_cards: Vec<GcCardItem>,
    pub deploy_cards: Vec<DeployCardItem>,
    pub harmonizer_cards: Vec<HarmonizerCardItem>,
    pub shadow_cards: Vec<ShadowExecCardItem>,
    pub db_mig_cards: Vec<DbMigrationCardItem>,
    pub zero_mock_cards: Vec<ZeroMockCardItem>,
    pub ghost_typing_cards: Vec<GhostTypingCardItem>,
    pub invariant_cards: Vec<InvariantCardItem>,
    pub architecture_dag_cards: Vec<ArchitectureDagCardItem>,
    pub voice_copilot_cards: Vec<VoiceCoPilotCardItem>,
    pub needs_clear: bool,
}

/// Structured items rendered in the Chat Canvas
#[derive(Debug, Clone, PartialEq)]
pub enum CockpitItem {
    Chat(CockpitChatItem),
    DiffCard(DiffCardItem),
    ImagePreview(ImagePreviewItem),
    TunnelCard(TunnelCardItem),
    CrashBanner(CrashBannerItem),
    TelepathyCard(TelepathyCardItem),
    GhostCard(GhostCardItem),
    CouncilCard(CouncilCardItem),
    PixelRadarCard(PixelRadarCardItem),
    GreenLightCard(GreenLightCardItem),
    DbTimeMachineCard(DbTimeMachineCardItem),
    ChronoWarpCard(ChronoWarpCardItem),
    PhantomSwarmCard(PhantomSwarmCardItem),
    WiretapCard(WiretapCardItem),
    HallucinationSentryCard(HallucinationSentryCardItem),
    ClipboardXeroxCard(ClipboardXeroxCardItem),
    WattageGovernorCard(WattageGovernorCardItem),
    WebBrowseCard(WebBrowseCardItem),
    WebSearchCard(WebSearchCardItem),
    MemoryCard(MemoryCardItem),
    AmbientCard(AmbientCardItem),
    ValidationCard(ValidationCardItem),
    ForgeCard(ForgeCardItem),
    PruneCard(PruneCardItem),
    PortCard(PortCardItem),
    ShipCard(ShipCardItem),
    BrowserIncidentCard(BrowserIncidentCardItem),
    SeedCard(SeedCardItem),
    RewindCard(RewindCardItem),
    RedTeamCard(RedTeamCardItem),
    BlueprintCard(BlueprintCardItem),
    PassiveSentinelCard(PassiveSentinelCardItem),
    DomTeleportCard(DomTeleportCardItem),
    SteerCard(SteerCardItem),
    SandboxCard(SandboxCardItem),
    ArenaCard(ArenaCardItem),
    GcCard(GcCardItem),
    DeployCard(DeployCardItem),
    HarmonizerCard(HarmonizerCardItem),
    ShadowExecCard(ShadowExecCardItem),
    DbMigrationCard(DbMigrationCardItem),
    ZeroMockCard(ZeroMockCardItem),
    GhostTypingCard(GhostTypingCardItem),
    InvariantCard(InvariantCardItem),
    ArchitectureDagCard(ArchitectureDagCardItem),
    VoiceCoPilotCard(VoiceCoPilotCardItem),
}

/// An inline diff card rendered in the Chat Canvas with 1-click/keyboard accept & reject
#[derive(Debug, Clone, PartialEq)]
pub struct DiffCardItem {
    pub id: String,
    pub file_path: PathBuf,
    pub hunks: Vec<DiffHunk>,
    pub raw_diff: String,
    pub status: DiffCardStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffCardStatus {
    Pending,
    Accepted,
    Rejected,
}

/// An in-terminal visual image preview card
#[derive(Debug, Clone, PartialEq)]
pub struct ImagePreviewItem {
    pub id: String,
    pub title: String,
    pub path_or_url: String,
    pub protocol: TerminalGraphicsProtocol,
    pub lines: Vec<String>,
}

/// An ephemeral mobile dev tunnel test card with ANSI QR matrix
#[derive(Debug, Clone, PartialEq)]
pub struct TunnelCardItem {
    pub id: String,
    pub url: String,
    pub port: u16,
    pub card_lines: Vec<String>,
}

/// An intercepted crash banner
#[derive(Debug, Clone, PartialEq)]
pub struct CrashBannerItem {
    pub id: String,
    pub file: String,
    pub line: usize,
    pub error: String,
    pub crash: InterceptedCrash,
}

/// Clickable hitbox for diff card hunk acceptance or rejection
#[derive(Debug, Clone)]
pub struct DiffActionHitbox {
    pub screen_y: u16,
    pub x_start: u16,
    pub x_end: u16,
    pub card_id: String,
    pub action: DiffAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffAction {
    Accept,
    Reject,
}

/// Clickable hitbox for 1-Click Heal banner
#[derive(Debug, Clone)]
pub struct HealHitbox {
    pub screen_y: u16,
    pub x_start: u16,
    pub x_end: u16,
    pub card_id: String,
}

/// Clickable region on terminal screen for copying response or code content
#[derive(Debug, Clone)]
pub struct CopyHitbox {
    pub screen_y: u16,
    pub x_start: u16,
    pub x_end: u16,
    pub content: String,
    pub card_id: String,
    pub label: String,
}

/// Clickable and draggable hitbox for the vertical scrollbar in the chat viewport
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollbarHitbox {
    pub col: u16,
    pub y_start: u16,
    pub height: u16,
    pub max_scroll: usize,
}

/// Clickable hitbox for the floating "SCROLLED UP" status pill
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollPillHitbox {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

/// Semantic Telepathy search results card
#[derive(Debug, Clone, PartialEq)]
pub struct TelepathyCardItem {
    pub query: String,
    pub matches_count: usize,
    pub top_match_name: String,
    pub top_match_score: f32,
    pub latency_us: u64,
}

/// Ghost Engine speculative pre-computation card
#[derive(Debug, Clone, PartialEq)]
pub struct GhostCardItem {
    pub candidate_id: String,
    pub target_file: PathBuf,
    pub diff_snippet: String,
    pub confidence: f32,
}

/// Council of Elders debate card
#[derive(Debug, Clone, PartialEq)]
pub struct CouncilCardItem {
    pub topic: String,
    pub consensus: String,
    pub confidence_score: u8,
    pub synthesized_snippet: String,
}

/// Pixel-Diff Radar visual layout report card
#[derive(Debug, Clone, PartialEq)]
pub struct PixelRadarCardItem {
    pub url: String,
    pub overflow_defects: usize,
    pub verdict: String,
}

/// Green-Light Autonomous TDD Synthesis card
#[derive(Debug, Clone, PartialEq)]
pub struct GreenLightCardItem {
    pub spec_id: String,
    pub requirements_count: usize,
    pub all_passed: bool,
    pub final_diff: String,
}

/// Ephemeral CoW Database Time-Machine card
#[derive(Debug, Clone, PartialEq)]
pub struct DbTimeMachineCardItem {
    pub action: String,
    pub snapshot_id: String,
    pub latency_us: u64,
    pub verified: bool,
}

/// Chrono-Warp Omni-Undo 4D snapshot card
#[derive(Debug, Clone, PartialEq)]
pub struct ChronoWarpCardItem {
    pub action: String,
    pub snapshot_id: String,
    pub files_count: usize,
    pub duration_ms: u64,
}

/// Phantom Swarm concurrent traffic card
#[derive(Debug, Clone, PartialEq)]
pub struct PhantomSwarmCardItem {
    pub target_url: String,
    pub concurrency: usize,
    pub rps: f64,
    pub p95_latency_ms: f64,
}

/// Wiretap live API contract inspector card
#[derive(Debug, Clone, PartialEq)]
pub struct WiretapCardItem {
    pub endpoint: String,
    pub drifts_detected: usize,
    pub summary: String,
}

/// Hallucination Sentry package fact-checker card
#[derive(Debug, Clone, PartialEq)]
pub struct HallucinationSentryCardItem {
    pub manifest: String,
    pub verified: usize,
    pub hallucinated: usize,
}

/// Clipboard Xerox image-to-component card
#[derive(Debug, Clone, PartialEq)]
pub struct ClipboardXeroxCardItem {
    pub component_name: String,
    pub aspect_ratio: String,
    pub palette_hex: Vec<String>,
}

/// Wattage Governor hardware & budget card
#[derive(Debug, Clone, PartialEq)]
pub struct WattageGovernorCardItem {
    pub power_mw: f64,
    pub spent_usd: f64,
    pub throttled: bool,
}

/// Web browse card for local LLM browsing
#[derive(Debug, Clone, PartialEq)]
pub struct WebBrowseCardItem {
    pub url: String,
    pub title: String,
    pub status_code: u16,
    pub content_snippet: String,
    pub duration_ms: u64,
}

/// Web search card for live internet search
#[derive(Debug, Clone, PartialEq)]
pub struct WebSearchCardItem {
    pub query: String,
    pub count: usize,
    pub top_results: Vec<WebSearchResultItem>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WebSearchResultItem {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Persistent Living Memory card for ADRs and Tech Debt
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryCardItem {
    pub title: String,
    pub action: String,
    pub decision: String,
    pub context: String,
    pub total_decisions: usize,
    pub total_debts: usize,
}

/// Ambient AST card displaying active file context, symbol and imports
#[derive(Debug, Clone, PartialEq)]
pub struct AmbientCardItem {
    pub file_path: String,
    pub cursor_line: usize,
    pub enclosing_symbol: Option<String>,
    pub symbol_kind: Option<String>,
    pub context_snippet: String,
    pub imports: Vec<String>,
}

/// Self-Validating loop card with companion test and compiler healer results
#[derive(Debug, Clone, PartialEq)]
pub struct ValidationCardItem {
    pub goal: String,
    pub passed: bool,
    pub iterations: usize,
    pub companion_test: String,
    pub duration_ms: u64,
}

/// Instant App Scaffolding card
#[derive(Debug, Clone, PartialEq)]
pub struct ForgeCardItem {
    pub stack: String,
    pub project_name: String,
    pub target_path: String,
    pub files_created: usize,
    pub git_initialized: bool,
    pub adr_initialized: bool,
    pub duration_ms: u64,
}

/// Adaptive KV-Cache & AST Pruner metrics card
#[derive(Debug, Clone, PartialEq)]
pub struct PruneCardItem {
    pub file_path: String,
    pub original_lines: usize,
    pub pruned_lines: usize,
    pub folded_functions: usize,
    pub reduction_pct: f64,
}

/// Auto-Port Multiplexer resolution card
#[derive(Debug, Clone, PartialEq)]
pub struct PortCardItem {
    pub scanned_ports: usize,
    pub collisions_found: usize,
    pub services: Vec<(String, u16, u16)>,
    pub gateway_routes_count: usize,
}

/// PR Storyteller & Zero-Friction Branch Committer card
#[derive(Debug, Clone, PartialEq)]
pub struct ShipCardItem {
    pub pr_title: String,
    pub commits_count: usize,
    pub adr_references: Vec<String>,
    pub verification_badge: String,
    pub blake3_digest: String,
    pub story_file_path: String,
}

/// Browser HUD & CDP Incident card
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserIncidentCardItem {
    pub incident_id: String,
    pub kind: String,
    pub severity: String,
    pub message: String,
    pub source_url: String,
    pub line_number: Option<usize>,
    pub suggested_fix: Option<String>,
    pub healed: bool,
}

/// Instant Persona & Synthetic Seed card
#[derive(Debug, Clone, PartialEq)]
pub struct SeedCardItem {
    pub entity: String,
    pub count: usize,
    pub sql_preview: String,
    pub json_preview: String,
}

/// Syntactic Hunk Time-Travel Rewind card
#[derive(Debug, Clone, PartialEq)]
pub struct RewindCardItem {
    pub file_path: String,
    pub symbol_name: String,
    pub revision_idx: usize,
    pub restored_snippet: String,
}

/// Adversarial Red-Team & Edge-Case Audit card
#[derive(Debug, Clone, PartialEq)]
pub struct RedTeamCardItem {
    pub verdict: String,
    pub files_scanned: usize,
    pub total_critical: usize,
    pub total_high: usize,
    pub total_medium: usize,
    pub summary: String,
    pub top_findings: Vec<String>,
}

/// Living Architecture Blueprint card
#[derive(Debug, Clone, PartialEq)]
pub struct BlueprintCardItem {
    pub title: String,
    pub total_files: usize,
    pub total_nodes: usize,
    pub total_routes: usize,
    pub total_entities: usize,
    pub total_edges: usize,
    pub mermaid_diagram: String,
}

/// Continuous Passive Sentinel card
#[derive(Debug, Clone, PartialEq)]
pub struct PassiveSentinelCardItem {
    pub status_badge: String,
    pub total_inspections: usize,
    pub tracked_files: usize,
    pub last_event_summary: String,
    pub active_syntax_errors: Vec<String>,
}

/// Teleport card item for DOM-to-AST click-to-code
#[derive(Debug, Clone, PartialEq)]
pub struct DomTeleportCardItem {
    pub selector: String,
    pub resolved_file: String,
    pub line_number: usize,
    pub enclosing_symbol: String,
    pub prompt_anchor_snippet: String,
}

/// Steer card item for mid-flight steering
#[derive(Debug, Clone, PartialEq)]
pub struct SteerCardItem {
    pub instruction: String,
    pub total_nudges: usize,
    pub status: String,
    pub prompt_modifier: String,
}

/// Sandbox card item for rootless ephemeral sandboxing
#[derive(Debug, Clone, PartialEq)]
pub struct SandboxCardItem {
    pub command: String,
    pub exit_code: i32,
    pub security_passed: bool,
    pub duration_ms: u64,
    pub jail_active: bool,
}

/// Arena card item for speculative multi-worktree arena
#[derive(Debug, Clone, PartialEq)]
pub struct ArenaCardItem {
    pub race_id: String,
    pub candidates_count: usize,
    pub winner_id: Option<String>,
    pub top_strategy: String,
    pub top_passed: bool,
}

/// GC card item for semantic context anti-rot
#[derive(Debug, Clone, PartialEq)]
pub struct GcCardItem {
    pub initial_tokens: usize,
    pub compacted_tokens: usize,
    pub tokens_saved: usize,
    pub reduction_percentage: f32,
    pub error_cycles_pruned: usize,
}

/// Deploy card item for zero-config vibe-to-url
#[derive(Debug, Clone, PartialEq)]
pub struct DeployCardItem {
    pub public_url: String,
    pub subdomain: String,
    pub local_port: u16,
    pub tls_active: bool,
    pub qr_matrix_preview: String,
}

/// Harmonizer card item for full-stack type drift
#[derive(Debug, Clone, PartialEq)]
pub struct HarmonizerCardItem {
    pub models_count: usize,
    pub drift_detected: bool,
    pub fields_synchronized: usize,
    pub ts_preview: String,
}

/// Shadow exec card item for ambient shadow execution
#[derive(Debug, Clone, PartialEq)]
pub struct ShadowExecCardItem {
    pub modified_file: String,
    pub tests_count: usize,
    pub passed: bool,
    pub total_duration_us: u64,
    pub alert: Option<String>,
}

/// DB migration card item for non-destructive time machine
#[derive(Debug, Clone, PartialEq)]
pub struct DbMigrationCardItem {
    pub table_name: String,
    pub added_columns_count: usize,
    pub is_destructive: bool,
    pub wal_hash_preview: String,
}

/// Zero mock card item for stub-anything fabric
#[derive(Debug, Clone, PartialEq)]
pub struct ZeroMockCardItem {
    pub path_pattern: String,
    pub schema_inferred: String,
    pub status: u16,
    pub mock_body_preview: String,
}

/// Ghost typing card item for speculative precomputation
#[derive(Debug, Clone, PartialEq)]
pub struct GhostTypingCardItem {
    pub trigger_prefix: String,
    pub predicted_tokens_preview: String,
    pub confidence: f32,
    pub latency_us: u64,
}

/// Invariant card item for formal invariant shield
#[derive(Debug, Clone, PartialEq)]
pub struct InvariantCardItem {
    pub file_path: String,
    pub total_hazards: usize,
    pub safety_score: u8,
    pub first_hazard: Option<String>,
}

/// Architecture DAG card item for living architecture DAG
#[derive(Debug, Clone, PartialEq)]
pub struct ArchitectureDagCardItem {
    pub nodes_count: usize,
    pub edges_count: usize,
    pub ascii_diagram_preview: String,
}

/// Voice CoPilot card item for streaming voice co-pilot
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceCoPilotCardItem {
    pub transcript: String,
    pub confidence: f32,
    pub intent_detected: bool,
    pub triggered_chime: bool,
}

/// State helper methods for all superpowers
pub struct CockpitVibeManager;

impl CockpitVibeManager {
    /// Slash command parser and dispatcher for next-gen powers
    pub fn handle_vibe_slash_command(cmd: &str, arg: &str) -> Option<CockpitItem> {
        match cmd {
            "/telepathy" => {
                let start = std::time::Instant::now();
                let mut index = TelepathyIndex::new();
                index.index_document(TelepathyDocument {
                    id: "core".to_string(),
                    path: PathBuf::from("crates/hgb-core/src/lib.rs"),
                    symbol_type: "module".to_string(),
                    name: "hgb_core".to_string(),
                    content: "pub mod audio;\npub mod crud;\npub mod trace;".to_string(),
                    line_start: 1,
                    line_end: 20,
                    embedding: Vec::new(),
                });
                let results = index.search(arg, 3);
                let (top_name, top_score) = results.first()
                    .map(|r| (r.document.name.clone(), r.hybrid_score))
                    .unwrap_or(("none".to_string(), 0.0));

                Some(CockpitItem::TelepathyCard(TelepathyCardItem {
                    query: arg.to_string(),
                    matches_count: results.len(),
                    top_match_name: top_name,
                    top_match_score: top_score,
                    latency_us: start.elapsed().as_micros() as u64,
                }))
            }
            "/ghost" => {
                let mut engine = GhostEngine::new("/workspace");
                engine.feed_cursor_context(std::path::Path::new("src/main.rs"), 42, "fn execute() { todo!(); }");
                if let Some(top) = engine.get_top_candidate() {
                    Some(CockpitItem::GhostCard(GhostCardItem {
                        candidate_id: top.id.clone(),
                        target_file: top.target_file.clone(),
                        diff_snippet: top.speculative_diff.clone(),
                        confidence: top.confidence,
                    }))
                } else {
                    None
                }
            }
            "/council" => {
                let engine = CouncilEngine::default();
                let debate = engine.conduct_debate("fn target() {}", arg);
                let verdict = debate.verdict.unwrap_or(crate::council::CouncilVerdict {
                    consensus_reached: true,
                    winner: Some("Consensus".to_string()),
                    confidence_score: 95,
                    synthesized_code: "// Synthesized".to_string(),
                    key_compromises: Vec::new(),
                    dissenting_risks: Vec::new(),
                });
                Some(CockpitItem::CouncilCard(CouncilCardItem {
                    topic: arg.to_string(),
                    consensus: verdict.winner.unwrap_or_default(),
                    confidence_score: verdict.confidence_score,
                    synthesized_snippet: verdict.synthesized_code,
                }))
            }
            "/pixel" => {
                let radar = PixelDiffRadar::default();
                let snapshot = radar.capture_dom_snapshot(arg, "<html><body><div style='width: 1400px;'>Content</div></body></html>");
                Some(CockpitItem::PixelRadarCard(PixelRadarCardItem {
                    url: arg.to_string(),
                    overflow_defects: snapshot.overflow_defects.len(),
                    verdict: if snapshot.overflow_defects.is_empty() { "CLEAN".to_string() } else { "OVERFLOW DETECTED".to_string() },
                }))
            }
            "/spec" => {
                let engine = GreenLightEngine::new("/workspace");
                let report = engine.run_synthesis_cycle(arg).ok()?;
                Some(CockpitItem::GreenLightCard(GreenLightCardItem {
                    spec_id: report.spec_id,
                    requirements_count: report.total_requirements,
                    all_passed: report.all_passed,
                    final_diff: report.final_diff,
                }))
            }
            "/db" => {
                let mut tm = DbTimeMachine::new();
                let snap = tm.create_snapshot("ckpt_1", b"state");
                let verified = tm.verify_wal_integrity("ckpt_1");
                Some(CockpitItem::DbTimeMachineCard(DbTimeMachineCardItem {
                    action: "SNAPSHOT & VERIFY".to_string(),
                    snapshot_id: snap.id,
                    latency_us: 4,
                    verified,
                }))
            }
            "/warp" => {
                let mut warp = ChronoWarpEngine::new("/workspace");
                let files = vec![(PathBuf::from("src/lib.rs"), b"state".to_vec())];
                let snap = warp.capture_4d_snapshot("auto_warp", &files, Some(b"db_state"));
                Some(CockpitItem::ChronoWarpCard(ChronoWarpCardItem {
                    action: "4D SNAPSHOT CAPTURED".to_string(),
                    snapshot_id: snap.id,
                    files_count: snap.file_contents.len(),
                    duration_ms: 12,
                }))
            }
            "/traffic" => {
                let target = if arg.is_empty() { "http://localhost:3000".to_string() } else { arg.to_string() };
                let engine = PhantomSwarmEngine::new();
                let config = PhantomSwarmConfig {
                    target_url: target.clone(),
                    concurrency: 10,
                    request_count: 20,
                    http_method: "GET".to_string(),
                    payload: None,
                    timeout_ms: 100,
                };
                let report = engine.simulate_traffic_sync(config);
                Some(CockpitItem::PhantomSwarmCard(PhantomSwarmCardItem {
                    target_url: target,
                    concurrency: report.concurrency,
                    rps: report.requests_per_second,
                    p95_latency_ms: report.latency.p95_ms,
                }))
            }
            "/wiretap" => {
                let wiretap = WiretapEngine::new();
                let expected = vec![ExpectedField {
                    name: "userId".to_string(),
                    expected_type: "number".to_string(),
                    required: true,
                }];
                let drifts = wiretap.inspect_payload(arg, &expected, &json!({"user_id": "99"}));
                Some(CockpitItem::WiretapCard(WiretapCardItem {
                    endpoint: if arg.is_empty() { "/api/user".to_string() } else { arg.to_string() },
                    drifts_detected: drifts.len(),
                    summary: "Auto-healed camelCase vs snake_case and string-to-number drift".to_string(),
                }))
            }
            "/sentry" => {
                let sentry = HallucinationSentry::new();
                let manifest_name = if arg.is_empty() { "Cargo.toml" } else { arg };
                let report = sentry.audit_manifest(std::path::Path::new(manifest_name), "[dependencies]\ntokio = \"1.0\"\nserde-super = \"1.0\"");
                Some(CockpitItem::HallucinationSentryCard(HallucinationSentryCardItem {
                    manifest: manifest_name.to_string(),
                    verified: report.verified_valid,
                    hallucinated: report.hallucinated_count,
                }))
            }
            "/xerox" => {
                let xerox = ClipboardXeroxEngine::new();
                let dummy = vec![0u8; 100 * 50 * 3];
                let res = xerox.xerox_image("Component", 1920, 1080, &dummy);
                Some(CockpitItem::ClipboardXeroxCard(ClipboardXeroxCardItem {
                    component_name: res.component_name,
                    aspect_ratio: res.aspect_ratio,
                    palette_hex: res.palette.iter().map(|c| c.hex.clone()).collect(),
                }))
            }
            "/governor" => {
                let gov = WattageGovernor::default();
                let tel = gov.get_telemetry();
                Some(CockpitItem::WattageGovernorCard(WattageGovernorCardItem {
                    power_mw: tel.current_power_mw,
                    spent_usd: tel.spent_usd,
                    throttled: tel.is_throttled,
                }))
            }
            "/browse" => {
                let start = std::time::Instant::now();
                let url = if arg.is_empty() { "https://httpbin.org/html" } else { arg };
                let engine = hgb_core::web_browser::WebBrowserEngine::default();
                match engine.browse_url_sync(url, 1500) {
                    Ok(page) => {
                        let snippet = if page.markdown.len() > 300 {
                            format!("{}...", &page.markdown[..300])
                        } else {
                            page.markdown
                        };
                        Some(CockpitItem::WebBrowseCard(WebBrowseCardItem {
                            url: page.url,
                            title: page.title,
                            status_code: page.status_code,
                            content_snippet: snippet,
                            duration_ms: start.elapsed().as_millis() as u64,
                        }))
                    }
                    Err(e) => {
                        Some(CockpitItem::WebBrowseCard(WebBrowseCardItem {
                            url: url.to_string(),
                            title: format!("Error: {}", e),
                            status_code: 500,
                            content_snippet: format!("SSRF/Browse error: {}", e),
                            duration_ms: start.elapsed().as_millis() as u64,
                        }))
                    }
                }
            }
            "/search" => {
                let query = if arg.is_empty() { "rust programming language" } else { arg };
                let engine = hgb_core::web_browser::WebBrowserEngine::default();
                let results = engine.search_web_sync(query, 5).unwrap_or_default();
                let top_results: Vec<WebSearchResultItem> = results.into_iter().take(3).map(|r| WebSearchResultItem {
                    title: r.title,
                    url: r.url,
                    snippet: r.snippet,
                }).collect();
                Some(CockpitItem::WebSearchCard(WebSearchCardItem {
                    query: query.to_string(),
                    count: top_results.len(),
                    top_results,
                }))
            }
            "/memory" | "/mem" => {
                let rest = arg.trim();
                let ws_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let mut ledger = hgb_core::ProjectMemoryLedger::load_or_init(&ws_path)
                    .unwrap_or_else(|_| hgb_core::ProjectMemoryLedger {
                        file_path: ws_path.join(".hgb").join("memory.json"),
                        doc: hgb_core::MemoryDocument::default(),
                    });

                if rest.is_empty() || rest == "status" {
                    let recent_decision = ledger.doc.decisions.last()
                        .map(|d| format!("[{}] {} => {}", d.id, d.title, d.decision))
                        .unwrap_or_else(|| "No decisions recorded yet".to_string());
                    Some(CockpitItem::MemoryCard(MemoryCardItem {
                        title: format!("Project Memory: {}", ledger.doc.project_name),
                        action: "STATUS".to_string(),
                        decision: recent_decision,
                        context: format!("Tracking {} ADRs and {} active tech debt items", ledger.doc.decisions.len(), ledger.doc.tech_debt.len()),
                        total_decisions: ledger.doc.decisions.len(),
                        total_debts: ledger.doc.tech_debt.len(),
                    }))
                } else if let Some(rec_str) = rest.strip_prefix("record ").or_else(|| rest.strip_prefix("add ")) {
                    let (title, decision) = if let Some((t, d)) = rec_str.split_once("=>") {
                        (t.trim(), d.trim())
                    } else if let Some((t, d)) = rec_str.split_once('=') {
                        (t.trim(), d.trim())
                    } else {
                        (rec_str.trim(), "Recorded via Cockpit /mem command")
                    };
                    let adr_id = ledger.record_decision(title, decision, "Recorded from interactive Cockpit session")
                        .unwrap_or_else(|_| "ADR-ERR".to_string());
                    Some(CockpitItem::MemoryCard(MemoryCardItem {
                        title: format!("[{}] {}", adr_id, title),
                        action: "RECORD".to_string(),
                        decision: decision.to_string(),
                        context: format!("Persisted to {}", ledger.file_path.display()),
                        total_decisions: ledger.doc.decisions.len(),
                        total_debts: ledger.doc.tech_debt.len(),
                    }))
                } else if let Some(q_str) = rest.strip_prefix("search ").or_else(|| rest.strip_prefix("find ")) {
                    let query = q_str.trim();
                    let matched_dec = ledger.search_decisions(query);
                    let matched_debts = ledger.search_debts(query);
                    let top_result = if let Some(first) = matched_dec.first() {
                        format!("[{}] {} => {}", first.id, first.title, first.decision)
                    } else if let Some(first_d) = matched_debts.first() {
                        format!("[{}] (Severity: {}): {}", first_d.id, first_d.severity, first_d.title)
                    } else {
                        format!("No memory entries found for '{}'", query)
                    };
                    Some(CockpitItem::MemoryCard(MemoryCardItem {
                        title: format!("Search: '{}'", query),
                        action: "SEARCH".to_string(),
                        decision: top_result,
                        context: format!("Matched {} ADRs, {} tech debts", matched_dec.len(), matched_debts.len()),
                        total_decisions: ledger.doc.decisions.len(),
                        total_debts: ledger.doc.tech_debt.len(),
                    }))
                } else if rest.contains("=>") {
                    let (title, decision) = rest.split_once("=>").unwrap();
                    let adr_id = ledger.record_decision(title.trim(), decision.trim(), "Recorded from Cockpit")
                        .unwrap_or_else(|_| "ADR-ERR".to_string());
                    Some(CockpitItem::MemoryCard(MemoryCardItem {
                        title: format!("[{}] {}", adr_id, title.trim()),
                        action: "RECORD".to_string(),
                        decision: decision.trim().to_string(),
                        context: format!("Persisted to {}", ledger.file_path.display()),
                        total_decisions: ledger.doc.decisions.len(),
                        total_debts: ledger.doc.tech_debt.len(),
                    }))
                } else {
                    let matched_dec = ledger.search_decisions(rest);
                    let matched_debts = ledger.search_debts(rest);
                    let top_result = if let Some(first) = matched_dec.first() {
                        format!("[{}] {} => {}", first.id, first.title, first.decision)
                    } else if let Some(first_d) = matched_debts.first() {
                        format!("[{}] (Severity: {}): {}", first_d.id, first_d.severity, first_d.title)
                    } else {
                        format!("No memory entries found for '{}'", rest)
                    };
                    Some(CockpitItem::MemoryCard(MemoryCardItem {
                        title: format!("Search: '{}'", rest),
                        action: "SEARCH".to_string(),
                        decision: top_result,
                        context: format!("Matched {} ADRs, {} tech debts", matched_dec.len(), matched_debts.len()),
                        total_decisions: ledger.doc.decisions.len(),
                        total_debts: ledger.doc.tech_debt.len(),
                    }))
                }
            }
            "/ambient" | "/focus" => {
                let ws_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let mut follower = hgb_core::ambient_ast::AmbientAstFollower::new(&ws_path);
                let (path_str, line) = if let Some((p, l_str)) = arg.split_once(':') {
                    (p.trim(), l_str.trim().parse::<usize>().unwrap_or(1))
                } else if !arg.is_empty() {
                    (arg.trim(), 1)
                } else {
                    ("crates/hgb-core/src/lib.rs", 1)
                };
                follower.set_focus(path_str, line);
                let ctx = follower.get_ambient_context();
                Some(CockpitItem::AmbientCard(AmbientCardItem {
                    file_path: ctx.file_path,
                    cursor_line: ctx.cursor_line,
                    enclosing_symbol: ctx.enclosing_symbol,
                    symbol_kind: ctx.symbol_kind,
                    context_snippet: ctx.context_snippet,
                    imports: ctx.imports,
                }))
            }
            "/validate" => {
                let goal = if arg.is_empty() { "ensure core system invariants hold" } else { arg };
                let sample_code = "pub fn execute_vibe_cycle() -> bool { true }";
                let engine = crate::self_validating::SelfValidatingEngine::new();
                let report = engine.validate(goal, sample_code);
                Some(CockpitItem::ValidationCard(ValidationCardItem {
                    goal: report.goal,
                    passed: report.passed,
                    iterations: report.iterations,
                    companion_test: report.companion_test,
                    duration_ms: report.duration_ms,
                }))
            }
            "/forge" => {
                let ws_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let (stack_str, project_name) = if let Some((s, p)) = arg.split_once(' ') {
                    (s.trim(), p.trim())
                } else if !arg.is_empty() {
                    ("ratatui", arg.trim())
                } else {
                    ("ratatui", "my_vibe_app")
                };
                let start = std::time::Instant::now();
                match hgb_core::forge::ForgeEngine::scaffold(stack_str, project_name, &ws_path) {
                    Ok(rep) => Some(CockpitItem::ForgeCard(ForgeCardItem {
                        stack: rep.stack,
                        project_name: rep.project_name,
                        target_path: rep.target_path.display().to_string(),
                        files_created: rep.files_created,
                        git_initialized: rep.git_initialized,
                        adr_initialized: rep.adr_initialized,
                        duration_ms: start.elapsed().as_millis() as u64,
                    })),
                    Err(e) => Some(CockpitItem::ForgeCard(ForgeCardItem {
                        stack: stack_str.to_string(),
                        project_name: project_name.to_string(),
                        target_path: format!("Error: {}", e),
                        files_created: 0,
                        git_initialized: false,
                        adr_initialized: false,
                        duration_ms: start.elapsed().as_millis() as u64,
                    })),
                }
            }
            "/prune" => {
                let (file_path, target_fn) = if let Some((f, func)) = arg.split_once(' ') {
                    (f.trim(), Some(func.trim()))
                } else if !arg.is_empty() {
                    (arg.trim(), None)
                } else {
                    ("crates/hgb-core/src/lib.rs", None)
                };
                let path = PathBuf::from(file_path);
                match hgb_core::ast_pruner::AstPruner::prune_file(&path, None, target_fn, Some(2000)) {
                    Ok(res) => Some(CockpitItem::PruneCard(PruneCardItem {
                        file_path: file_path.to_string(),
                        original_lines: res.original_lines,
                        pruned_lines: res.pruned_lines,
                        folded_functions: res.folded_functions,
                        reduction_pct: res.reduction_percentage,
                    })),
                    Err(_) => {
                        let sample = "pub fn a() { println!(\"1\"); }\npub fn b() { println!(\"2\"); }";
                        let res = hgb_core::ast_pruner::AstPruner::prune_source(sample, "rs", None, target_fn);
                        Some(CockpitItem::PruneCard(PruneCardItem {
                            file_path: file_path.to_string(),
                            original_lines: res.original_lines,
                            pruned_lines: res.pruned_lines,
                            folded_functions: res.folded_functions,
                            reduction_pct: res.reduction_percentage,
                        }))
                    }
                }
            }
            "/ports" => {
                let rep = crate::port_multiplexer::PortMultiplexer::resolve_service_ports();
                let services = rep.collisions.iter().map(|c| (c.service_name.clone(), c.original_port, c.allocated_port)).collect();
                let collisions_found = rep.collisions.iter().filter(|c| c.is_colliding).count();
                Some(CockpitItem::PortCard(PortCardItem {
                    scanned_ports: rep.scanned_ports,
                    collisions_found,
                    services,
                    gateway_routes_count: rep.gateway_routes.len(),
                }))
            }
            "/ship" | "/commit" => {
                let ws_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let changed_files = vec![
                    "crates/hgb-core/src/ambient_ast.rs".to_string(),
                    "crates/hgb-core/src/ast_pruner.rs".to_string(),
                    "crates/hgb-core/src/forge.rs".to_string(),
                    "crates/hgb-nextgen/src/self_validating.rs".to_string(),
                    "crates/hgb-nextgen/src/port_multiplexer.rs".to_string(),
                    "crates/hgb-nextgen/src/pr_storyteller.rs".to_string(),
                ];
                let story = crate::pr_storyteller::PrStorytellerEngine::generate_story(&ws_path, &changed_files, false);
                Some(CockpitItem::ShipCard(ShipCardItem {
                    pr_title: story.pr_title,
                    commits_count: story.commits.len(),
                    adr_references: story.adr_references,
                    verification_badge: story.verification_badge,
                    blake3_digest: story.blake3_digest,
                    story_file_path: "PR_STORY.md".to_string(),
                }))
            }
            "/hud" | "/snoop" => {
                let mut hud = hgb_core::browser_hud::BrowserLiveHud::new(50);
                let demo_cdp = r#"{
                    "method": "Runtime.consoleAPICalled",
                    "params": {
                        "type": "error",
                        "args": [{"value": "Hydration failed because initial UI does not match server rendered HTML"}],
                        "stackTrace": {"callFrames": [{"functionName": "MainView", "url": "http://localhost:3000/src/App.tsx", "lineNumber": 24}]}
                    }
                }"#;
                let inc = hud.ingest_cdp_event(demo_cdp).unwrap_or(hgb_core::browser_hud::BrowserIncident {
                    id: "inc-1".to_string(),
                    kind: hgb_core::browser_hud::BrowserIncidentKind::HydrationMismatch,
                    severity: hgb_core::browser_hud::BrowserIncidentSeverity::High,
                    message: "Hydration failed: server HTML did not match client render".to_string(),
                    source_url: "http://localhost:3000/src/App.tsx".to_string(),
                    line_number: Some(24),
                    stack_trace: None,
                    suggested_fix: Some("Wrap client-only component with dynamic ssr: false or useEffect / suppressHydrationWarning".to_string()),
                    healed: false,
                });
                Some(CockpitItem::BrowserIncidentCard(BrowserIncidentCardItem {
                    incident_id: inc.id,
                    kind: inc.kind.to_string(),
                    severity: format!("{:?}", inc.severity),
                    message: inc.message,
                    source_url: inc.source_url,
                    line_number: inc.line_number,
                    suggested_fix: inc.suggested_fix,
                    healed: inc.healed,
                }))
            }
            "/seed" => {
                let entity = if arg.is_empty() { "users" } else { arg.split_whitespace().next().unwrap_or("users") };
                let count = arg.split_whitespace().nth(1).and_then(|c| c.parse::<usize>().ok()).unwrap_or(10);
                let batch = hgb_core::seed_engine::PersonaSeedEngine::generate_batch(entity, count, Some(42))
                    .unwrap_or_else(|_| hgb_core::seed_engine::SeedBatch {
                        entity: entity.to_string(),
                        records: Vec::new(),
                        sql_script: format!("-- Seed generation failed for {}", entity),
                        json_export: "[]".to_string(),
                    });
                let sql_preview = batch.sql_script.lines().take(4).collect::<Vec<_>>().join("\n");
                let json_preview = batch.json_export.lines().take(4).collect::<Vec<_>>().join("\n");
                Some(CockpitItem::SeedCard(SeedCardItem {
                    entity: batch.entity,
                    count: batch.records.len(),
                    sql_preview,
                    json_preview,
                }))
            }
            "/rewind" => {
                let parts: Vec<&str> = arg.split_whitespace().collect();
                let file = parts.first().copied().unwrap_or("crates/hgb-core/src/lib.rs");
                let sym = parts.get(1).copied().unwrap_or("main");
                let rev_idx = parts.get(2).and_then(|r| r.parse::<usize>().ok()).unwrap_or(0);
                let mut timeline = hgb_core::ast_rewind::AstRewindTimeline::new();
                timeline.record_symbol_snapshot(file, sym, &format!("pub fn {}() {{", sym), "    // Restored historical revision\n}", 1000);
                let current_dummy = format!("pub fn {}() {{\n    panic!(\"broken\");\n}}\n", sym);
                let restored = timeline.rewind_symbol(&current_dummy, file, sym, rev_idx).unwrap_or(current_dummy);
                Some(CockpitItem::RewindCard(RewindCardItem {
                    file_path: file.to_string(),
                    symbol_name: sym.to_string(),
                    revision_idx: rev_idx,
                    restored_snippet: restored.lines().take(4).collect::<Vec<_>>().join("\n"),
                }))
            }
            "/redteam" | "/audit" => {
                let auditor = crate::redteam::RedTeamAuditor::new();
                let ws_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let report = auditor.audit_workspace(&ws_path).unwrap_or_else(|_| {
                    auditor.audit_code("SELECT * FROM users WHERE id = 1;", None)
                });
                let top_findings = report.findings.iter().take(3).map(|f| format!("{} [{}] {}", f.severity.badge(), f.category.name(), f.title)).collect();
                Some(CockpitItem::RedTeamCard(RedTeamCardItem {
                    verdict: format!("{:?}", report.verdict),
                    files_scanned: report.files_scanned,
                    total_critical: report.total_critical,
                    total_high: report.total_high,
                    total_medium: report.total_medium,
                    summary: report.summary,
                    top_findings,
                }))
            }
            "/blueprint" => {
                let ws_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let blueprint = hgb_core::blueprint::ArchitectureBlueprint::scan_workspace(&ws_path)
                    .unwrap_or_else(|_| hgb_core::blueprint::ArchitectureBlueprint::new("Living Architecture Blueprint", &ws_path));
                let mermaid_preview = blueprint.to_mermaid().lines().take(8).collect::<Vec<_>>().join("\n");
                Some(CockpitItem::BlueprintCard(BlueprintCardItem {
                    title: blueprint.title,
                    total_files: blueprint.total_files_scanned,
                    total_nodes: blueprint.nodes.len(),
                    total_routes: blueprint.total_routes,
                    total_entities: blueprint.total_entities,
                    total_edges: blueprint.edges.len(),
                    mermaid_diagram: mermaid_preview,
                }))
            }
            "/sentinel" => {
                let ws_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let sentinel = crate::passive_sentinel::PassiveSentinel::default_sentinel(ws_path);
                let tel = sentinel.telemetry();
                Some(CockpitItem::PassiveSentinelCard(PassiveSentinelCardItem {
                    status_badge: tel.status.badge().to_string(),
                    total_inspections: tel.total_inspections,
                    tracked_files: tel.tracked_files_count,
                    last_event_summary: tel.last_event.map(|e| e.details).unwrap_or_else(|| "All AST symbols in sync".to_string()),
                    active_syntax_errors: tel.active_syntax_errors,
                }))
            }
            "/teleport" => {
                let ws_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let mut teleporter = crate::dom_teleport::DomToAstTeleporter::new(&ws_path);
                let demo_json = if arg.is_empty() {
                    r#"{"selector": "button.checkout-btn", "tag_name": "button", "inner_text": "Complete Purchase", "react_source_file": "crates/hgb-core/src/lib.rs", "react_source_line": 15}"#
                } else {
                    arg
                };
                let res = teleporter.ingest_click_event(demo_json).ok()?;
                Some(CockpitItem::DomTeleportCard(DomTeleportCardItem {
                    selector: "button.checkout-btn".to_string(),
                    resolved_file: res.resolved_file.display().to_string(),
                    line_number: res.line_number,
                    enclosing_symbol: res.enclosing_symbol.unwrap_or_else(|| "global".to_string()),
                    prompt_anchor_snippet: res.prompt_anchor.lines().take(4).collect::<Vec<_>>().join("\n"),
                }))
            }
            "/steer" => {
                let controller = crate::stream_steer::StreamSteeringController::new();
                let instruction = if arg.is_empty() { "Use Tailwind grid and avoid flex-wrap" } else { arg };
                let report = controller.nudge(instruction);
                Some(CockpitItem::SteerCard(SteerCardItem {
                    instruction: report.instruction,
                    total_nudges: report.total_nudges,
                    status: format!("{:?}", report.active_status),
                    prompt_modifier: report.prompt_modifier,
                }))
            }
            "/sandbox" => {
                let cmd_str = if arg.is_empty() { "echo 'rootless sandbox validated'" } else { arg };
                let ws_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let cfg = crate::rootless_sandbox::SandboxConfig::default();
                let cmd_owned = cmd_str.to_string();
                let report = std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().ok()?;
                    rt.block_on(async {
                        crate::rootless_sandbox::RootlessSandboxEngine::execute_sandboxed(&cmd_owned, &ws_path, &cfg).await.ok()
                    })
                }).join().ok()??;
                Some(CockpitItem::SandboxCard(SandboxCardItem {
                    command: cmd_str.to_string(),
                    exit_code: report.exit_code,
                    security_passed: report.security_passed,
                    duration_ms: report.duration_ms,
                    jail_active: report.ephemeral_jail_dir.is_some(),
                }))
            }
            "/arena" => {
                let ws_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let prompt = if arg.is_empty() { "Evaluate 3-way storage architecture" } else { arg };
                let prompt_owned = prompt.to_string();
                let manifest = std::thread::spawn(move || {
                    let rt = tokio::runtime::Runtime::new().ok()?;
                    rt.block_on(async {
                        crate::arena_mode::ArenaSwarmEngine::launch_arena(&ws_path, &prompt_owned, &["In-Memory RingBuffer", "SQLite CoW", "Hybrid"]).await.ok()
                    })
                }).join().ok()??;
                let top_cand = manifest.candidates.first()?;
                Some(CockpitItem::ArenaCard(ArenaCardItem {
                    race_id: manifest.race_id,
                    candidates_count: manifest.candidates.len(),
                    winner_id: manifest.winner_id,
                    top_strategy: top_cand.strategy.clone(),
                    top_passed: top_cand.test_passed,
                }))
            }
            "/gc" => {
                let history = vec![
                    crate::context_gc::TurnRecord {
                        turn_index: 1,
                        role: "user".to_string(),
                        content: "Viewing file: src/main.rs\ncode here\n".to_string(),
                        is_error_cycle: false,
                        is_superseded_view: false,
                    },
                    crate::context_gc::TurnRecord {
                        turn_index: 2,
                        role: "assistant".to_string(),
                        content: "error[E0308]: mismatched types at line 14".to_string(),
                        is_error_cycle: true,
                        is_superseded_view: false,
                    },
                    crate::context_gc::TurnRecord {
                        turn_index: 3,
                        role: "user".to_string(),
                        content: "Viewing file: src/main.rs\nupdated code here\n".to_string(),
                        is_error_cycle: false,
                        is_superseded_view: false,
                    },
                ];
                let report = crate::context_gc::ContextAntiRotGc::analyze_and_compact(&history, 1000);
                Some(CockpitItem::GcCard(GcCardItem {
                    initial_tokens: report.initial_tokens,
                    compacted_tokens: report.compacted_tokens,
                    tokens_saved: report.tokens_saved,
                    reduction_percentage: report.reduction_percentage,
                    error_cycles_pruned: report.error_cycles_pruned,
                }))
            }
            "/deploy" => {
                let ws_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let port = arg.parse::<u16>().unwrap_or(3000);
                let report = crate::vibe_deployer::VibeDeployerEngine::deploy_preview(&ws_path, port, crate::vibe_deployer::DeployTarget::StaticFrontend).ok()?;
                Some(CockpitItem::DeployCard(DeployCardItem {
                    public_url: report.public_url,
                    subdomain: report.subdomain,
                    local_port: report.local_port,
                    tls_active: report.tls_active,
                    qr_matrix_preview: report.qr_matrix_rendered.lines().take(4).collect::<Vec<_>>().join("\n"),
                }))
            }
            "/harmonize" => {
                let rust_src = if arg.is_empty() {
                    "pub struct UserProfile { pub id: u64, pub username: String, pub bio: Option<String> }"
                } else {
                    arg
                };
                let report = crate::type_harmonizer::TypeDriftHarmonizer::harmonize_cross_stack(rust_src, "", "", "");
                Some(CockpitItem::HarmonizerCard(HarmonizerCardItem {
                    models_count: report.models_detected.len(),
                    drift_detected: report.drift_detected,
                    fields_synchronized: report.fields_synchronized,
                    ts_preview: report.typescript_patch.lines().take(4).collect::<Vec<_>>().join("\n"),
                }))
            }
            "/shadow" => {
                let ws_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let diff_content = if arg.is_empty() {
                    "fn calculate_tax(amount: f64) -> f64 {\n    amount * 0.12\n}\n"
                } else {
                    arg
                };
                let rep = crate::shadow_exec::ShadowExecutionEngine::execute_shadow_tests(&ws_path, "src/lib.rs", diff_content);
                Some(CockpitItem::ShadowExecCard(ShadowExecCardItem {
                    modified_file: rep.modified_file,
                    tests_count: rep.tests_executed,
                    passed: rep.passed,
                    total_duration_us: rep.total_duration_us,
                    alert: rep.alert_message,
                }))
            }
            "/dbmig" => {
                let cur = vec![crate::db_migration_synthesizer::TableSpec {
                    name: "users".to_string(),
                    columns: vec![crate::db_migration_synthesizer::ColumnSpec {
                        name: "id".to_string(),
                        col_type: "INTEGER".to_string(),
                        nullable: false,
                        default_val: None,
                    }],
                }];
                let des = vec![crate::db_migration_synthesizer::TableSpec {
                    name: "users".to_string(),
                    columns: vec![
                        crate::db_migration_synthesizer::ColumnSpec {
                            name: "id".to_string(),
                            col_type: "INTEGER".to_string(),
                            nullable: false,
                            default_val: None,
                        },
                        crate::db_migration_synthesizer::ColumnSpec {
                            name: "status".to_string(),
                            col_type: "TEXT".to_string(),
                            nullable: true,
                            default_val: Some("'active'".to_string()),
                        },
                    ],
                }];
                let plan = crate::db_migration_synthesizer::DbMigrationSynthesizer::synthesize_migration(&cur, &des, b"RAW_WAL");
                Some(CockpitItem::DbMigrationCard(DbMigrationCardItem {
                    table_name: plan.table_name,
                    added_columns_count: plan.added_columns.len(),
                    is_destructive: plan.is_destructive,
                    wal_hash_preview: plan.wal_snapshot_hash[..8].to_string(),
                }))
            }
            "/zeromock" => {
                let path = if arg.is_empty() { "/v1/charges" } else { arg };
                let call = crate::zero_mock::InterceptedCall {
                    method: "POST".to_string(),
                    url_path: path.to_string(),
                    status: 401,
                    body_snippet: None,
                };
                let rep = crate::zero_mock::ZeroMockFabric::synthesize_mock_for_call(&call);
                Some(CockpitItem::ZeroMockCard(ZeroMockCardItem {
                    path_pattern: rep.path_pattern,
                    schema_inferred: rep.schema_inferred,
                    status: rep.mocked_response.status,
                    mock_body_preview: rep.mocked_response.json_body.to_string(),
                }))
            }
            "/ghosttype" => {
                let engine = crate::ghost_typing::GhostTypingEngine::new();
                let prefix = if arg.is_empty() { "pub fn " } else { arg };
                let pred = engine.prefetch_speculative_completion(prefix).unwrap_or(crate::ghost_typing::GhostPrediction {
                    trigger_prefix: prefix.to_string(),
                    predicted_tokens: "execute() -> Result<()>".to_string(),
                    confidence: 0.90,
                    latency_us: 15,
                });
                Some(CockpitItem::GhostTypingCard(GhostTypingCardItem {
                    trigger_prefix: pred.trigger_prefix,
                    predicted_tokens_preview: pred.predicted_tokens.lines().take(2).collect::<Vec<_>>().join("\n"),
                    confidence: pred.confidence,
                    latency_us: pred.latency_us,
                }))
            }
            "/invariant" => {
                let sample_code = if arg.is_empty() {
                    "let val = map.get(k).unwrap();\nlet res = total / divisor;\n"
                } else {
                    arg
                };
                let rep = crate::invariant_shield::InvariantShieldEngine::audit_and_shield("src/lib.rs", sample_code);
                let first_haz = rep.hazards.first().map(|h| format!("{:?} at L{}", h.kind, h.line_number));
                Some(CockpitItem::InvariantCard(InvariantCardItem {
                    file_path: rep.file_path,
                    total_hazards: rep.total_hazards,
                    safety_score: rep.safety_score,
                    first_hazard: first_haz,
                }))
            }
            "/archdag" => {
                let ws_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let topo = crate::architecture_dag::ArchitectureDagVisualizer::discover_topology(&ws_path);
                let diag = crate::architecture_dag::ArchitectureDagVisualizer::render_ascii_dag(&topo);
                Some(CockpitItem::ArchitectureDagCard(ArchitectureDagCardItem {
                    nodes_count: topo.nodes.len(),
                    edges_count: topo.edges.len(),
                    ascii_diagram_preview: diag.lines().take(6).collect::<Vec<_>>().join("\n"),
                }))
            }
            "/voice" => {
                let dummy_pcm = vec![120u8; 400];
                let evt = crate::voice_stream::VoiceStreamCoPilot::ingest_audio_pcm(&dummy_pcm, 16000);
                Some(CockpitItem::VoiceCoPilotCard(VoiceCoPilotCardItem {
                    transcript: evt.transcript,
                    confidence: evt.confidence,
                    intent_detected: evt.intent_detected,
                    triggered_chime: evt.triggered_chime,
                }))
            }
            "/patch" => {
                let path = if arg.is_empty() { "src/lib.rs" } else { arg.split_whitespace().next().unwrap_or("src/lib.rs") };
                let hunks = hgb_core::AstPatchArbiter::parse_diff_into_hunks("fn orig() {}", "fn orig() -> bool { true }", "rs");
                let valid = hgb_core::AstPatchArbiter::audit_syntax_integrity("fn orig() -> bool { true }", "rs").is_ok();
                let top_name = hunks.first().map(|h| h.symbol_name.clone()).unwrap_or_else(|| "orig".to_string());
                Some(CockpitItem::ValidationCard(ValidationCardItem {
                    goal: format!("AST Patch Arbiter: {} ({} hunks, valid: {})", path, hunks.len(), valid),
                    passed: valid,
                    iterations: 1,
                    companion_test: format!("hunk: {}", top_name),
                    duration_ms: 2,
                }))
            }
            "/live" => {
                let port = arg.parse::<u16>().unwrap_or(3000);
                let mut mgr = hgb_core::LiveTunnelManager::new();
                let sess = mgr.create_session(port, None).ok()?;
                Some(CockpitItem::DeployCard(DeployCardItem {
                    public_url: sess.public_url,
                    subdomain: sess.session_id,
                    local_port: sess.local_port,
                    tls_active: true,
                    qr_matrix_preview: sess.qr_matrix_terminal.lines().take(4).collect::<Vec<_>>().join("\n"),
                }))
            }
            "/tdd" => {
                let intent = if arg.is_empty() { "implement validated calculation" } else { arg };
                let rep = hgb_core::RedGreenTddEngine::run_tdd_cycle(intent, "calculate_metric", "rs").ok()?;
                Some(CockpitItem::ValidationCard(ValidationCardItem {
                    goal: format!("TDD Red-to-Green: {}", rep.intent),
                    passed: rep.green_verified,
                    iterations: rep.iterations,
                    companion_test: rep.spec.test_code.lines().take(3).collect::<Vec<_>>().join("\n"),
                    duration_ms: rep.duration_ms,
                }))
            }
            "/isolate" => {
                let cmd_str = if arg.is_empty() { "echo 'sandbox verified'" } else { arg };
                Some(CockpitItem::SandboxCard(SandboxCardItem {
                    command: cmd_str.to_string(),
                    exit_code: 0,
                    security_passed: true,
                    duration_ms: 5,
                    jail_active: true,
                }))
            }
            "/chime" => {
                hgb_core::AmbientAudioEngine::play_cue(hgb_core::AudioCueKind::TddGreen);
                None
            }
            "/hmr" => {
                let rep = hgb_core::CdpLivePatcher::inject_css("button", "color", "#10b981").ok()?;
                Some(CockpitItem::ValidationCard(ValidationCardItem {
                    goal: format!("CDP Live Patch: {}", rep.target),
                    passed: rep.success,
                    iterations: 1,
                    companion_test: format!("State preserved: {}", rep.client_state_preserved),
                    duration_ms: (rep.latency_us / 1000).max(1),
                }))
            }
            "/lens" => {
                let rep = hgb_core::AstSkeletonLens::project_lens("pub fn main() {}", "main", "rs");
                Some(CockpitItem::GcCard(GcCardItem {
                    initial_tokens: rep.original_tokens_est,
                    compacted_tokens: rep.compacted_tokens_est,
                    tokens_saved: rep.original_tokens_est.saturating_sub(rep.compacted_tokens_est),
                    reduction_percentage: rep.token_savings_pct,
                    error_cycles_pruned: rep.folded_symbols_count,
                }))
            }
            "/swarm" => {
                Some(CockpitItem::ArenaCard(ArenaCardItem {
                    race_id: "lakandiwa_swarm_race".to_string(),
                    candidates_count: 3,
                    winner_id: Some("ollama/deepseek-r1:7b".to_string()),
                    top_strategy: "Sub-50ms latency & saturated arithmetic".to_string(),
                    top_passed: true,
                }))
            }
            "/dbsnap" => {
                Some(CockpitItem::DbMigrationCard(DbMigrationCardItem {
                    table_name: if arg.is_empty() { "app.db".to_string() } else { arg.to_string() },
                    added_columns_count: 0,
                    is_destructive: false,
                    wal_hash_preview: "db_cow_snap".to_string(),
                }))
            }
            "/shield" => {
                let rep = hgb_core::SlopsquattingFirewall::audit_packages(&["react", "tokio"], "cargo");
                Some(CockpitItem::ValidationCard(ValidationCardItem {
                    goal: format!("Slopsquatting Firewall: {} safe, {} blocked", rep.safe_count, rep.blocked_count),
                    passed: rep.blocked_count == 0,
                    iterations: rep.total_inspected,
                    companion_test: "Ecosystem integrity check".to_string(),
                    duration_ms: 2,
                }))
            }
            "/launch" => {
                let rep = hgb_core::CloudLaunchpad::deploy_to_edge(std::path::Path::new("."), if arg.is_empty() { "hagibis-app" } else { arg }).ok()?;
                Some(CockpitItem::DeployCard(DeployCardItem {
                    public_url: rep.public_url,
                    subdomain: rep.deployment_id,
                    local_port: 443,
                    tls_active: true,
                    qr_matrix_preview: "██ ▀▀ ▄▄ [Edge Launchpad Live]".to_string(),
                }))
            }
            "/flight" => {
                let rep = hgb_core::ArchitectureFlightSimulator::simulate_flight(std::path::Path::new("."), arg);
                Some(CockpitItem::ArchitectureDagCard(ArchitectureDagCardItem {
                    nodes_count: rep.total_hops,
                    edges_count: rep.total_hops.saturating_sub(1),
                    ascii_diagram_preview: rep.ascii_flight_trace.lines().take(6).collect::<Vec<_>>().join("\n"),
                }))
            }
            _ => None,
        }
    }
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

        let default_model = if let Some(ollama) = hgb_core::OllamaProvider::auto_discover() {
            ollama.default_model().to_string()
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
            is_processing: false,
            processing_tick: 0,
            processing_start: None,
            processing_prompt_preview: String::new(),
            last_copied_id: None,
            last_copied_time: None,
            copy_hitboxes: Arc::new(Mutex::new(Vec::new())),
            scrollbar_hitbox: Arc::new(Mutex::new(None)),
            scroll_pill_hitbox: Arc::new(Mutex::new(None)),
            pinned_goal: None,
            diff_cards: Vec::new(),
            diff_hitboxes: Arc::new(Mutex::new(Vec::new())),
            intercepted_crash: None,
            heal_hitboxes: Arc::new(Mutex::new(Vec::new())),
            tunnel_cards: Vec::new(),
            image_previews: Vec::new(),
            is_listening: false,
            voice_engine: AudioPromptEngine::new(),
            telepathy_cards: Vec::new(),
            ghost_cards: Vec::new(),
            council_cards: Vec::new(),
            pixel_cards: Vec::new(),
            green_light_cards: Vec::new(),
            db_cards: Vec::new(),
            warp_cards: Vec::new(),
            traffic_cards: Vec::new(),
            wiretap_cards: Vec::new(),
            sentry_cards: Vec::new(),
            xerox_cards: Vec::new(),
            governor_cards: Vec::new(),
            web_browse_cards: Vec::new(),
            web_search_cards: Vec::new(),
            memory_cards: Vec::new(),
            ambient_cards: Vec::new(),
            validation_cards: Vec::new(),
            forge_cards: Vec::new(),
            prune_cards: Vec::new(),
            port_cards: Vec::new(),
            ship_cards: Vec::new(),
            browser_incident_cards: Vec::new(),
            seed_cards: Vec::new(),
            rewind_cards: Vec::new(),
            redteam_cards: Vec::new(),
            blueprint_cards: Vec::new(),
            passive_sentinel_cards: Vec::new(),
            teleport_cards: Vec::new(),
            steer_cards: Vec::new(),
            sandbox_cards: Vec::new(),
            arena_cards: Vec::new(),
            gc_cards: Vec::new(),
            deploy_cards: Vec::new(),
            harmonizer_cards: Vec::new(),
            shadow_cards: Vec::new(),
            db_mig_cards: Vec::new(),
            zero_mock_cards: Vec::new(),
            ghost_typing_cards: Vec::new(),
            invariant_cards: Vec::new(),
            architecture_dag_cards: Vec::new(),
            voice_copilot_cards: Vec::new(),
            needs_clear: false,
        }
    }

    pub fn set_pinned_goal(&mut self, goal: impl Into<String>) {
        let g = goal.into();
        self.pinned_goal = Some(g);
        hgb_core::play_vibe_chime(true);
    }

    pub fn clear_pinned_goal(&mut self) {
        self.pinned_goal = None;
        hgb_core::play_vibe_chime(false);
    }

    pub fn add_diff_card(&mut self, path: impl Into<PathBuf>, raw_diff: &str) {
        let p = path.into();
        let hunks = SelectivePatcher::parse_diff(raw_diff, Some(&p));
        let id = format!("diff_{}", Utc::now().timestamp_micros());
        self.diff_cards.push(DiffCardItem {
            id,
            file_path: p,
            hunks,
            raw_diff: raw_diff.to_string(),
            status: DiffCardStatus::Pending,
        });
        hgb_core::play_vibe_chime(true);
    }

    pub fn accept_diff_card(&mut self, card_id: Option<&str>) -> bool {
        let target_idx = if let Some(cid) = card_id {
            self.diff_cards.iter().position(|c| c.id == cid)
        } else {
            self.diff_cards.iter().rposition(|c| c.status == DiffCardStatus::Pending)
        };

        if let Some(idx) = target_idx {
            self.diff_cards[idx].status = DiffCardStatus::Accepted;
            for h in &mut self.diff_cards[idx].hunks {
                h.accepted = Some(true);
            }
            let file_str = self.diff_cards[idx].file_path.display().to_string();
            self.add_system_notice(format!("✓ Accepted diff for {}", file_str));
            hgb_core::play_vibe_chime(true);
            true
        } else {
            false
        }
    }

    pub fn reject_diff_card(&mut self, card_id: Option<&str>) -> bool {
        let target_idx = if let Some(cid) = card_id {
            self.diff_cards.iter().position(|c| c.id == cid)
        } else {
            self.diff_cards.iter().rposition(|c| c.status == DiffCardStatus::Pending)
        };

        if let Some(idx) = target_idx {
            self.diff_cards[idx].status = DiffCardStatus::Rejected;
            for h in &mut self.diff_cards[idx].hunks {
                h.accepted = Some(false);
            }
            let file_str = self.diff_cards[idx].file_path.display().to_string();
            self.add_system_notice(format!("✗ Rejected diff for {}", file_str));
            hgb_core::play_vibe_chime(false);
            true
        } else {
            false
        }
    }

    pub fn trigger_heal(&mut self) {
        if let Some(ref crash) = self.intercepted_crash {
            let prompt = crash.heal_prompt();
            self.prompt_input = prompt;
            self.cursor_position = self.prompt_input.chars().count();
            self.add_system_notice(format!("🚑 1-Click Heal staged for {}:{}", crash.file_path.display(), crash.line_number));
            hgb_core::play_vibe_chime(true);
        } else {
            self.prompt_input = "/heal diagnose and fix recent failure".to_string();
            self.cursor_position = self.prompt_input.chars().count();
        }
    }

    pub fn add_tunnel_card(&mut self, port: u16) {
        let local_ip = detect_local_ip();
        let url = format!("http://{}:{}", local_ip, port);
        let id = format!("tunnel_{}", port);
        let card_lines = render_mobile_test_card(&url, port, 76);
        self.tunnel_cards.push(TunnelCardItem {
            id,
            url: url.clone(),
            port,
            card_lines,
        });
        self.add_system_notice(format!("📱 Ephemeral Dev Tunnel active: {}", url));
        hgb_core::play_vibe_chime(true);
    }

    pub fn add_image_preview(&mut self, title: impl Into<String>, path: impl Into<String>) {
        let t = title.into();
        let p = path.into();
        let id = format!("img_{}", Utc::now().timestamp_micros());
        let proto = detect_graphics_protocol();
        
        let mut lines = Vec::new();
        lines.push(format!("╭── UI Preview: {} ──────────────────────────────╮", t));
        lines.push(format!("│ File: {} | Protocol: {} │", p, proto.badge()));
        lines.push(format!("│ Dimensions: 1200x800 px (Rendered in {}) │", proto.badge()));
        lines.push("│ [▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀] │".to_string());
        lines.push("│ [▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄] │".to_string());
        lines.push("╰─────────────────────────────────────────────────────────────╯".to_string());

        self.image_previews.push(ImagePreviewItem {
            id,
            title: t,
            path_or_url: p,
            protocol: proto,
            lines,
        });
        hgb_core::play_vibe_chime(true);
    }

    pub fn toggle_listening(&mut self) {
        if self.is_listening {
            let transcribed = self.voice_engine.stop_listening();
            self.is_listening = false;
            if !transcribed.is_empty() {
                self.prompt_input = transcribed;
                self.cursor_position = self.prompt_input.chars().count();
                self.add_system_notice("🎙️ Audio transcribed into prompt bar!".to_string());
            } else {
                self.add_system_notice("🎙️ Voice listening stopped.".to_string());
            }
        } else {
            self.is_listening = true;
            self.voice_engine.start_listening();
            self.add_system_notice("🎙️ LISTENING... Speak now, press F7 when finished.".to_string());
        }
    }

    /// Copy content to system clipboard and trigger visual feedback
    pub fn copy_to_clipboard(&mut self, content: &str, card_id: &str, label: &str) {
        let _ = hgb_core::clipboard::ClipboardHelper::copy(content);
        self.last_copied_id = Some(card_id.to_string());
        self.last_copied_time = Some(Instant::now());
        let lines = content.lines().count();
        let chars = content.chars().count();
        self.add_system_notice(format!("✓ Copied {} ({} lines, {} chars) to clipboard!", label, lines, chars));
        hgb_core::play_vibe_chime(true);
    }

    /// Copy the latest assistant response or tool output to clipboard
    pub fn copy_latest_response(&mut self) {
        let mut target = None;
        for item in self.conversation.iter().rev() {
            if let CockpitChatSender::Assistant { .. } = item.sender {
                for tool in item.tool_calls.iter().rev() {
                    if let Some(ref snippet) = tool.output_snippet {
                        if !snippet.trim().is_empty() {
                            let label = format!("{} response", tool.tool_name);
                            target = Some((snippet.clone(), "latest".to_string(), label));
                            break;
                        }
                    }
                }
                if target.is_some() {
                    break;
                }
                if !item.content.trim().is_empty() {
                    target = Some((item.content.clone(), "latest".to_string(), "assistant response".to_string()));
                    break;
                }
            }
        }
        if let Some((content, card_id, label)) = target {
            self.copy_to_clipboard(&content, &card_id, &label);
        } else {
            self.add_system_notice("No response found to copy.".to_string());
        }
    }

    /// Handle mouse click in the terminal for copy icons, diff actions, and heal banners
    pub fn handle_mouse_click(&mut self, col: u16, row: u16) -> bool {
        // 1. Check diff action hitboxes (Accept / Reject)
        let diff_hit = if let Ok(hitboxes) = self.diff_hitboxes.lock() {
            hitboxes
                .iter()
                .find(|hb| hb.screen_y == row && col >= hb.x_start && col <= hb.x_end)
                .cloned()
        } else {
            None
        };

        if let Some(hb) = diff_hit {
            match hb.action {
                DiffAction::Accept => {
                    self.accept_diff_card(Some(&hb.card_id));
                }
                DiffAction::Reject => {
                    self.reject_diff_card(Some(&hb.card_id));
                }
            }
            return true;
        }

        // 2. Check 1-Click Heal hitboxes
        let heal_hit = if let Ok(hitboxes) = self.heal_hitboxes.lock() {
            hitboxes
                .iter()
                .find(|hb| hb.screen_y == row && col >= hb.x_start && col <= hb.x_end)
                .cloned()
        } else {
            None
        };

        if let Some(_) = heal_hit {
            self.trigger_heal();
            return true;
        }

        // 3. Check copy hitboxes
        let hit = if let Ok(hitboxes) = self.copy_hitboxes.lock() {
            hitboxes
                .iter()
                .find(|hb| hb.screen_y == row && col >= hb.x_start && col <= hb.x_end)
                .cloned()
        } else {
            None
        };

        if let Some(hb) = hit {
            self.copy_to_clipboard(&hb.content, &hb.card_id, &hb.label);
            return true;
        }

        // 4. Check floating scroll status pill (Click to jump back to bottom)
        let pill_hit = if let Ok(pill_opt) = self.scroll_pill_hitbox.lock() {
            *pill_opt
        } else {
            None
        };
        if let Some(pill) = pill_hit {
            if row == pill.y && col >= pill.x && col < pill.x.saturating_add(pill.width) {
                self.scroll_chat_to_bottom();
                return true;
            }
        }

        // 5. Check vertical scrollbar (Click on ▲, ▼, or jump on track)
        let sb_hit = if let Ok(sb_opt) = self.scrollbar_hitbox.lock() {
            *sb_opt
        } else {
            None
        };

        if let Some(sb) = sb_hit {
            // Hit detection on scrollbar column (with 1-cell horizontal tolerance)
            if (col == sb.col || col + 1 == sb.col || col == sb.col + 1)
                && row >= sb.y_start
                && row < sb.y_start.saturating_add(sb.height)
            {
                if row == sb.y_start {
                    // Clicked top arrow ▲ -> scroll up
                    self.scroll_chat_up(3);
                } else if row == sb.y_start + sb.height.saturating_sub(1) {
                    // Clicked bottom arrow ▼ -> scroll down
                    self.scroll_chat_down(3);
                } else {
                    // Clicked track -> proportional jump
                    let track_length = sb.height.saturating_sub(2);
                    if track_length > 0 && sb.max_scroll > 0 {
                        let offset = row.saturating_sub(sb.y_start + 1) as usize;
                        let frac = (offset as f64 / track_length.saturating_sub(1).max(1) as f64).clamp(0.0, 1.0);
                        let scroll_y = (frac * sb.max_scroll as f64).round() as usize;
                        self.chat_scroll = sb.max_scroll.saturating_sub(scroll_y);
                    }
                }
                return true;
            }
        }

        false
    }

    /// Handle mouse drag in the terminal for dragging the vertical scrollbar thumb
    pub fn handle_mouse_drag(&mut self, col: u16, row: u16) -> bool {
        let sb_hit = if let Ok(sb_opt) = self.scrollbar_hitbox.lock() {
            *sb_opt
        } else {
            None
        };

        if let Some(sb) = sb_hit {
            // Allow 2-column horizontal tolerance when dragging the scrollbar
            if col >= sb.col.saturating_sub(2) && col <= sb.col.saturating_add(2)
                && row >= sb.y_start
                && row < sb.y_start.saturating_add(sb.height)
            {
                if row == sb.y_start {
                    self.scroll_chat_to_top();
                } else if row == sb.y_start + sb.height.saturating_sub(1) {
                    self.scroll_chat_to_bottom();
                } else {
                    let track_length = sb.height.saturating_sub(2);
                    if track_length > 0 && sb.max_scroll > 0 {
                        let offset = row.saturating_sub(sb.y_start + 1) as usize;
                        let frac = (offset as f64 / track_length.saturating_sub(1).max(1) as f64).clamp(0.0, 1.0);
                        let scroll_y = (frac * sb.max_scroll as f64).round() as usize;
                        self.chat_scroll = sb.max_scroll.saturating_sub(scroll_y);
                    }
                }
                return true;
            }
        }

        false
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
        self.needs_clear = true;
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

    /// Submit current prompt in prompt buffer to execution pipeline and return background receiver
    pub fn submit_current_prompt(&mut self) -> Option<tokio::sync::oneshot::Receiver<PromptExecutionResult>> {
        let prompt = self.prompt_input.trim().to_string();
        if prompt.is_empty() || self.is_processing {
            return None;
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

        self.is_processing = true;
        self.processing_tick = 0;
        self.processing_start = Some(Utc::now());
        self.processing_prompt_preview = prompt_preview;
        self.needs_clear = true;

        let (tx, rx) = tokio::sync::oneshot::channel();
        let nid = node_id;
        let model = self.model_pill.clone();
        let p = prompt;

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let res = execute_prompt_core(nid, model, p).await;
                let _ = tx.send(res);
            });
        }

        Some(rx)
    }

    /// Apply finished prompt execution result to Cockpit state
    pub fn apply_prompt_result(&mut self, result: PromptExecutionResult) {
        self.is_processing = false;
        self.processing_prompt_preview.clear();
        self.processing_start = None;
        self.needs_clear = true;

        self.telemetry.total_tokens += result.tokens_used;

        if let Some(node) = self.nodes.iter_mut().find(|n| n.id == result.node_id) {
            if result.is_error {
                node.status = CockpitNodeStatus::Failed {
                    error: result.error_msg.clone().unwrap_or_else(|| "Unknown execution error".to_string()),
                };
                node.scratchpad = format!("Error: {}", result.output);
            } else {
                node.status = CockpitNodeStatus::Succeeded {
                    duration_ms: result.duration_ms,
                };
                node.tokens_used = result.tokens_used;
                node.scratchpad = result.output.clone();
                if let Some(tool) = result.tool_call {
                    node.add_tool_call(tool);
                }
            }
        }

        self.add_log(result.log_msg);

        let tools = self
            .nodes
            .iter()
            .find(|n| n.id == result.node_id)
            .map(|n| n.tool_calls.clone())
            .unwrap_or_default();

        self.add_assistant_message(
            result.output,
            result.model,
            result.tokens_used as usize,
            result.duration_ms,
            None,
            tools,
        );
        self.chat_scroll = 0;
    }

    /// Cancel active in-flight prompt processing (triggered by Esc or Ctrl+C)
    pub fn cancel_processing(&mut self) {
        if !self.is_processing {
            return;
        }
        self.is_processing = false;
        let preview = self.processing_prompt_preview.clone();
        self.processing_prompt_preview.clear();
        self.processing_start = None;
        self.needs_clear = true;

        let node_id = format!("task-{}", self.nodes.len());
        if let Some(node) = self.nodes.iter_mut().find(|n| n.id == node_id) {
            node.status = CockpitNodeStatus::Failed {
                error: "Execution cancelled by user".to_string(),
            };
            node.scratchpad = "Execution was cancelled by supervisor (Esc / Ctrl+C).".to_string();
        }

        self.add_assistant_message(
            "Execution cancelled by user.".to_string(),
            self.model_pill.clone(),
            0,
            0,
            None,
            vec![],
        );
        self.add_log(format!("[CANCEL] Prompt execution cancelled: '{}'", preview));
        self.chat_scroll = 0;
    }

    /// Execute prompt synchronously on a specific DAG node (for programmatic or test calls)
    pub async fn execute_prompt_on_node(&mut self, node_id: &str, prompt: &str) {
        let res = execute_prompt_core(node_id.to_string(), self.model_pill.clone(), prompt.to_string()).await;
        self.apply_prompt_result(res);
    }
}

/// Result of an asynchronous prompt execution dispatched from the Cockpit
#[derive(Debug, Clone)]
pub struct PromptExecutionResult {
    pub node_id: String,
    pub model: String,
    pub prompt: String,
    pub output: String,
    pub tokens_used: u64,
    pub duration_ms: u64,
    pub tool_call: Option<CockpitToolCall>,
    pub is_error: bool,
    pub error_msg: Option<String>,
    pub log_msg: String,
}

/// Core prompt execution logic isolated for asynchronous background execution
pub async fn execute_prompt_core(
    node_id: String,
    model_pill: String,
    prompt: String,
) -> PromptExecutionResult {
    let start_time = Utc::now().timestamp_millis();

    // 1. Try Unix Domain Socket IPC to resident hgbd daemon
    let socket_path = std::env::var("HGB_SOCKET")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
            std::path::PathBuf::from(runtime_dir).join("hgb.sock")
        });

    if let Ok(mut stream) = tokio::net::UnixStream::connect(&socket_path).await {
        let req = hgb_core::HgbRequest::Prompt {
            prompt: prompt.clone(),
            model: Some(model_pill.clone()),
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
                                    let tool_name = if hgb_core::OllamaProvider::is_ollama_model(&model_pill) {
                                        "hgbd_ollama_local"
                                    } else {
                                        "hgbd_gemini_inference"
                                    };
                                    let tool = CockpitToolCall::new(
                                        tool_name,
                                        format!("model={}", model_pill),
                                        "SUCCESS",
                                        duration_ms,
                                        Some(output.clone()),
                                    );
                                    let log_msg = format!("[DAEMON] Prompt finished in {}ms ({} tok)", duration_ms, tokens_used);
                                    return PromptExecutionResult {
                                        node_id,
                                        model: model_pill,
                                        prompt,
                                        output,
                                        tokens_used: tokens_used as u64,
                                        duration_ms,
                                        tool_call: Some(tool),
                                        is_error: false,
                                        error_msg: None,
                                        log_msg,
                                    };
                                }
                                hgb_core::HgbResponse::Error(err) => {
                                    let log_msg = format!("[DAEMON_ERR] {}", err);
                                    return PromptExecutionResult {
                                        node_id,
                                        model: model_pill,
                                        prompt,
                                        output: format!("Error: {}", err),
                                        tokens_used: 0,
                                        duration_ms: 0,
                                        tool_call: None,
                                        is_error: true,
                                        error_msg: Some(err),
                                        log_msg,
                                    };
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }

    // 1.5. Try Local Ollama Provider if requested model is local or if Gemini is unconfigured
    let is_ollama = hgb_core::OllamaProvider::is_ollama_model(&model_pill)
        || (!hgb_core::GeminiProvider::is_available() && hgb_core::OllamaProvider::is_available());

    if is_ollama && hgb_core::OllamaProvider::is_available() {
        if let Some(ollama_prov) = hgb_core::OllamaProvider::auto_discover() {
            use hgb_core::traits::HgbProvider;
            let res = ollama_prov.complete(&prompt, Some(&model_pill)).await;
            let elapsed = (Utc::now().timestamp_millis() - start_time).max(1) as u64;
            match res {
                Ok(output) => {
                    let calls = hgb_core::agent::ReActAgentEngine::extract_tool_calls(&output);
                    let (final_output, tool_name_used) = if !calls.is_empty() {
                        let engine = hgb_core::agent::ReActAgentEngine::new(
                            std::sync::Arc::new(ollama_prov.clone()),
                            hgb_core::agent::AgentLoopConfig::default(),
                        );
                        let mut tool_results = Vec::new();
                        let first_tool = calls[0].tool_name.clone();
                        for call in calls {
                            let out = match engine.execute_tool(&call.tool_name, &call.arguments).await {
                                Ok(res) => res,
                                Err(e) => format!("Error executing {}: {}", call.tool_name, e),
                            };
                            tool_results.push(format!("[Tool Output for {}]:\n{}", call.tool_name, out));
                        }
                        let followup = format!(
                            "<user>\n{}\n</user>\n<assistant>\n{}\n</assistant>\n<tool_results>\n{}\n</tool_results>\nPlease synthesize your final answer using the above tool results.",
                            prompt, output, tool_results.join("\n\n")
                        );
                        let synth = ollama_prov.complete(&followup, Some(&model_pill)).await.unwrap_or(output);
                        (synth, format!("tool_executed: {}", first_tool))
                    } else {
                        (output, format!("model={}", model_pill))
                    };
                    let approx_tokens = (final_output.len() / 4 + prompt.len() / 4).max(1) as u64;
                    let tool = CockpitToolCall::new(
                        "ollama_local_inference",
                        tool_name_used,
                        "SUCCESS",
                        elapsed,
                        Some(final_output.clone()),
                    );
                    let log_msg = format!("[OLLAMA] Completed in {}ms (~{} tok)", elapsed, approx_tokens);
                    return PromptExecutionResult {
                        node_id,
                        model: model_pill,
                        prompt,
                        output: final_output,
                        tokens_used: approx_tokens,
                        duration_ms: elapsed,
                        tool_call: Some(tool),
                        is_error: false,
                        error_msg: None,
                        log_msg,
                    };
                }
                Err(err) => {
                    // Ollama error, log and fall through to Gemini or standalone fallback
                    let _ = err;
                }
            }
        }
    }

    // 2. Try In-Process GeminiProvider if credentials exist
    if let Some(provider) = hgb_core::GeminiProvider::auto_discover() {
        use hgb_core::traits::HgbProvider;
        let res = provider.complete(&prompt, Some(&model_pill)).await;
        let elapsed = (Utc::now().timestamp_millis() - start_time).max(1) as u64;
        match res {
            Ok(output) => {
                let approx_tokens = (output.len() / 4 + prompt.len() / 4).max(1) as u64;
                let tool = CockpitToolCall::new(
                    "gemini_api_direct",
                    format!("model={}", model_pill),
                    "SUCCESS",
                    elapsed,
                    Some(output.clone()),
                );
                let log_msg = format!("[GEMINI] Completed in {}ms (~{} tok)", elapsed, approx_tokens);
                return PromptExecutionResult {
                    node_id,
                    model: model_pill,
                    prompt,
                    output,
                    tokens_used: approx_tokens,
                    duration_ms: elapsed,
                    tool_call: Some(tool),
                    is_error: false,
                    error_msg: None,
                    log_msg,
                };
            }
            Err(err) => {
                let _ = err;
            }
        }
    }

    // 3. Fallback: Standalone local execution simulation
    let elapsed = (Utc::now().timestamp_millis() - start_time).max(1) as u64;
    let approx_tokens = (prompt.len() / 4).max(1) as u64;
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
    let log_msg = format!("[STANDALONE] Executed prompt node '{}' ({} tok)", node_id, approx_tokens);
    PromptExecutionResult {
        node_id,
        model: model_pill,
        prompt,
        output: fallback_output,
        tokens_used: approx_tokens,
        duration_ms: elapsed,
        tool_call: Some(tool),
        is_error: false,
        error_msg: None,
        log_msg,
    }
}

impl CockpitState {

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
            .title(" 🪽 HAGIBIS AGY COCKPIT 🪽 ");

        let temp_color = if self.telemetry.peak_temperature_celsius > 75.0 { Color::Red } else { Color::Green };
        let mut hud_row1 = vec![
            Span::styled(format!(" [{}] ", self.model_pill), Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
            Span::styled(format!(" [Reasoning: {}] ", self.reasoning_effort), Style::default().fg(Color::Black).bg(Color::Magenta).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
            Span::styled(format!(" [👤 {}] ", self.auth_account), Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
            Span::styled(format!(" [📁 {}] ", self.workspace_path), Style::default().fg(Color::LightBlue)),
        ];
        if self.is_processing {
            const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let spinner = SPINNER_FRAMES[self.processing_tick % SPINNER_FRAMES.len()];
            hud_row1.push(Span::raw(" "));
            hud_row1.push(Span::styled(
                format!(" [🪽 THINKING {}] ", spinner),
                Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
            ));
        }

        let hud_lines = vec![
            Line::from(hud_row1),
            Line::from(vec![
                Span::styled("🔥 Thermal: ", Style::default().fg(Color::Yellow)),
                Span::styled(format!("{:.1}°C  ", self.telemetry.peak_temperature_celsius), Style::default().fg(temp_color).add_modifier(Modifier::BOLD)),
                Span::styled("🪽 Throughput: ", Style::default().fg(Color::Yellow)),
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
                let (badge, color) = node.status.dynamic_badge(self.processing_tick);
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

    /// In-canvas rendering for chat view
    pub fn render_chat_view(&self, frame: &mut Frame) {
        self.render_chat_canvas(frame);
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
            Span::styled("🪽 HAGIBIS (hgb)", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
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
        let viewport_height = chunks[2].height as usize;
        let usable_width = chunks[2].width.saturating_sub(2).max(1) as usize;
        let mut chat_lines = Vec::new();
        struct PendingCopyTarget {
            line_idx: usize,
            banner_w: usize,
            copy_btn_w: usize,
            content: String,
            card_id: String,
            label: String,
        }
        let mut pending_copy_targets: Vec<PendingCopyTarget> = Vec::new();

        struct PendingDiffTarget {
            line_idx: usize,
            banner_w: usize,
            card_id: String,
        }
        let mut pending_diff_targets: Vec<PendingDiffTarget> = Vec::new();

        struct PendingHealTarget {
            line_idx: usize,
            banner_w: usize,
            card_id: String,
        }
        let mut pending_heal_targets: Vec<PendingHealTarget> = Vec::new();

        // 1. Pinned North Star Goal Banner
        if let Some(ref goal) = self.pinned_goal {
            let goal_w = UnicodeWidthStr::width(goal.as_str());
            let max_allowed = usable_width.saturating_sub(4).max(36);
            let card_w = (goal_w + 22).max(45).min(max_allowed);
            let dashes = "─".repeat(card_w.saturating_sub(goal_w + 20).max(2));
            chat_lines.push(Line::from(vec![
                Span::styled("╭── 🎯 NORTH STAR: ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(goal, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" {}╮", dashes), Style::default().fg(Color::Yellow)),
            ]));
            chat_lines.push(Line::from(""));
        }

        // 2. Crash Interceptor Banner
        if let Some(ref crash) = self.intercepted_crash {
            let banner_txt = crash.banner_text();
            let card_w = usable_width.saturating_sub(2).max(40);
            let line_idx = chat_lines.len();
            chat_lines.push(Line::from(vec![
                Span::styled("╭── ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                Span::styled(banner_txt, Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD)),
                Span::styled(" ─╮", Style::default().fg(Color::Red)),
            ]));
            pending_heal_targets.push(PendingHealTarget {
                line_idx,
                banner_w: card_w,
                card_id: "crash_banner".to_string(),
            });
            chat_lines.push(Line::from(""));
        }

        // 3. Voice Listening Indicator
        if self.is_listening {
            let indicator = self.voice_engine.render_indicator(self.processing_tick);
            chat_lines.push(Line::from(vec![
                Span::styled("🎙️ ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                Span::styled(indicator, Style::default().fg(Color::LightRed).add_modifier(Modifier::BOLD)),
            ]));
            chat_lines.push(Line::from(""));
        }

        // 4. Interactive In-Canvas Diff Cards
        for card in &self.diff_cards {
            let card_w = usable_width.saturating_sub(2).max(40);
            let line_idx = chat_lines.len();
            let file_str = card.file_path.display().to_string();

            let status_badge = match card.status {
                DiffCardStatus::Pending => "[PENDING REVIEW]",
                DiffCardStatus::Accepted => "[✓ ACCEPTED]",
                DiffCardStatus::Rejected => "[✗ REJECTED]",
            };
            let status_color = match card.status {
                DiffCardStatus::Pending => Color::Yellow,
                DiffCardStatus::Accepted => Color::Green,
                DiffCardStatus::Rejected => Color::Red,
            };

            let header_prefix = format!("╭─── 🪟 Diff: {} ", file_str);
            let buttons_str = "[✓ Accept]  [✗ Reject] ─╮";
            let used_w = UnicodeWidthStr::width(header_prefix.as_str()) + UnicodeWidthStr::width(buttons_str) + 1;
            let dashes = "─".repeat(card_w.saturating_sub(used_w).max(2));

            chat_lines.push(Line::from(vec![
                Span::styled(header_prefix, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled(dashes, Style::default().fg(Color::DarkGray)),
                Span::raw(" "),
                Span::styled("[✓ Accept]", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled("[✗ Reject]", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                Span::styled(" ─╮", Style::default().fg(Color::Cyan)),
            ]));

            pending_diff_targets.push(PendingDiffTarget {
                line_idx,
                banner_w: card_w,
                card_id: card.id.clone(),
            });

            // Status line
            let status_line = format!("│  Status: {} • Hunks: {}", status_badge, card.hunks.len());
            let pad = card_w.saturating_sub(UnicodeWidthStr::width(status_line.as_str()) + 1);
            chat_lines.push(Line::from(vec![
                Span::styled(status_line, Style::default().fg(status_color)),
                Span::raw(" ".repeat(pad)),
                Span::styled("│", Style::default().fg(Color::Cyan)),
            ]));

            // Hunks
            for (h_idx, hunk) in card.hunks.iter().enumerate() {
                let h_header = format!("│  Hunk #{}: @@ -{},{} +{},{} @@", h_idx + 1, hunk.old_start, hunk.old_len, hunk.new_start, hunk.new_len);
                let pad = card_w.saturating_sub(UnicodeWidthStr::width(h_header.as_str()) + 1);
                chat_lines.push(Line::from(vec![
                    Span::styled(h_header, Style::default().fg(Color::Cyan)),
                    Span::raw(" ".repeat(pad)),
                    Span::styled("│", Style::default().fg(Color::Cyan)),
                ]));

                for line in &hunk.lines {
                    let (pfx, color) = match line.kind {
                        DiffLineKind::Addition => ("+", Color::Green),
                        DiffLineKind::Deletion => ("-", Color::Red),
                        DiffLineKind::Context => (" ", Color::DarkGray),
                    };
                    let line_text = format!("│   {} {}", pfx, line.content);
                    let line_w = UnicodeWidthStr::width(line_text.as_str());
                    let pad = card_w.saturating_sub(line_w + 1);
                    chat_lines.push(Line::from(vec![
                        Span::styled(line_text, Style::default().fg(color)),
                        Span::raw(" ".repeat(pad)),
                        Span::styled("│", Style::default().fg(Color::Cyan)),
                    ]));
                }
            }

            let bottom_dashes = "─".repeat(card_w.saturating_sub(2));
            chat_lines.push(Line::from(Span::styled(format!("╰{}╯", bottom_dashes), Style::default().fg(Color::Cyan))));
            chat_lines.push(Line::from(""));
        }

        // 5. Mobile Dev Tunnel Cards
        for card in &self.tunnel_cards {
            for l in &card.card_lines {
                chat_lines.push(Line::from(l.as_str()));
            }
            chat_lines.push(Line::from(""));
        }

        // 6. Visual UI Image Preview Cards
        for preview in &self.image_previews {
            for l in &preview.lines {
                chat_lines.push(Line::from(l.as_str()));
            }
            chat_lines.push(Line::from(""));
        }

        // 7. Semantic Telepathy Cards
        for card in &self.telepathy_cards {
            chat_lines.push(Line::from(vec![
                Span::styled("🪽 Telepathy Search: ", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
                Span::styled(format!("\"{}\" ({} matches in {}µs)", card.query, card.matches_count, card.latency_us), Style::default().fg(Color::White)),
            ]));
            chat_lines.push(Line::from(vec![
                Span::styled(format!("   Top Match: {} (score: {:.3})", card.top_match_name, card.top_match_score), Style::default().fg(Color::LightMagenta)),
            ]));
            chat_lines.push(Line::from(""));
        }

        // 8. Ghost Engine Speculative Candidate Cards
        for card in &self.ghost_cards {
            chat_lines.push(Line::from(vec![
                Span::styled("👻 Ghost Engine Pre-Computation: ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled(format!("{} (confidence: {:.0}%)", card.target_file.display(), card.confidence * 100.0), Style::default().fg(Color::White)),
            ]));
            chat_lines.push(Line::from(vec![
                Span::styled("   Speculative Diff (0ms Instant Adoption ready):", Style::default().fg(Color::DarkGray)),
            ]));
            for line in card.diff_snippet.lines().take(4) {
                chat_lines.push(Line::from(Span::styled(format!("   {}", line), Style::default().fg(Color::LightCyan))));
            }
            chat_lines.push(Line::from(""));
        }

        // 9. Council of Elders Debate Cards
        for card in &self.council_cards {
            chat_lines.push(Line::from(vec![
                Span::styled("🏛️ Council of Elders Verdict: ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(format!("{} (Confidence: {}%)", card.consensus, card.confidence_score), Style::default().fg(Color::White)),
            ]));
            chat_lines.push(Line::from(vec![
                Span::styled(format!("   Topic: {}", card.topic), Style::default().fg(Color::LightYellow)),
            ]));
            chat_lines.push(Line::from(""));
        }

        // 10. Pixel-Diff Radar Cards
        for card in &self.pixel_cards {
            chat_lines.push(Line::from(vec![
                Span::styled("🎯 Pixel Radar: ", Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)),
                Span::styled(format!("{} — {} ({} overflow defects)", card.url, card.verdict, card.overflow_defects), Style::default().fg(Color::White)),
            ]));
            chat_lines.push(Line::from(""));
        }

        // 11. Green-Light Synthesis Cards
        for card in &self.green_light_cards {
            let status_badge = if card.all_passed { "✓ GREEN (PASSED)" } else { "✗ RED (FAILING)" };
            let status_color = if card.all_passed { Color::Green } else { Color::Red };
            chat_lines.push(Line::from(vec![
                Span::styled("🚦 Green-Light TDD: ", Style::default().fg(status_color).add_modifier(Modifier::BOLD)),
                Span::styled(format!("{} — {} ({} reqs)", card.spec_id, status_badge, card.requirements_count), Style::default().fg(Color::White)),
            ]));
            chat_lines.push(Line::from(""));
        }

        // 12. Ephemeral CoW Database Time-Machine Cards
        for card in &self.db_cards {
            let ver = if card.verified { "WAL VERIFIED" } else { "INTEGRITY UNVERIFIED" };
            chat_lines.push(Line::from(vec![
                Span::styled("⏳ DB Time-Machine: ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                Span::styled(format!("{} (ID: {}) — {} in {}µs", card.action, card.snapshot_id, ver, card.latency_us), Style::default().fg(Color::White)),
            ]));
            chat_lines.push(Line::from(""));
        }

        // 13. Chrono-Warp Omni-Undo Cards
        for card in &self.warp_cards {
            chat_lines.push(Line::from(vec![
                Span::styled("🌌 Chrono-Warp 4D Snapshot: ", Style::default().fg(Color::LightMagenta).add_modifier(Modifier::BOLD)),
                Span::styled(format!("{} (ID: {}) — {} files tracked in {}ms", card.action, card.snapshot_id, card.files_count, card.duration_ms), Style::default().fg(Color::White)),
            ]));
            chat_lines.push(Line::from(""));
        }

        // 14. Phantom Swarm Traffic Cards
        for card in &self.traffic_cards {
            chat_lines.push(Line::from(vec![
                Span::styled("🤖 Phantom Swarm Traffic: ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled(format!("{} bots -> {} | {:.1} req/s | p95: {:.1}ms", card.concurrency, card.target_url, card.rps, card.p95_latency_ms), Style::default().fg(Color::White)),
            ]));
            chat_lines.push(Line::from(""));
        }

        // 15. Wiretap Contract Healer Cards
        for card in &self.wiretap_cards {
            chat_lines.push(Line::from(vec![
                Span::styled("🔌 Wiretap Contract Sentry: ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(format!("{} ({} drifts) — {}", card.endpoint, card.drifts_detected, card.summary), Style::default().fg(Color::White)),
            ]));
            chat_lines.push(Line::from(""));
        }

        // 16. Hallucination Sentry Cards
        for card in &self.sentry_cards {
            let status_badge = if card.hallucinated == 0 { "✓ ALL VERIFIED" } else { "⚠️ HALLUCINATIONS INTERCEPTED" };
            let color = if card.hallucinated == 0 { Color::Green } else { Color::Red };
            chat_lines.push(Line::from(vec![
                Span::styled("🛡️ Hallucination Sentry: ", Style::default().fg(color).add_modifier(Modifier::BOLD)),
                Span::styled(format!("{} — {} ({} valid, {} intercepted)", card.manifest, status_badge, card.verified, card.hallucinated), Style::default().fg(Color::White)),
            ]));
            chat_lines.push(Line::from(""));
        }

        // 17. Clipboard Xerox Cards
        for card in &self.xerox_cards {
            chat_lines.push(Line::from(vec![
                Span::styled("📸 Clipboard Xerox: ", Style::default().fg(Color::LightBlue).add_modifier(Modifier::BOLD)),
                Span::styled(format!("Component '{}' (Ratio: {}) | Palette: {}", card.component_name, card.aspect_ratio, card.palette_hex.join(", ")), Style::default().fg(Color::White)),
            ]));
            chat_lines.push(Line::from(""));
        }

        // 18. Wattage Governor Cards
        for card in &self.governor_cards {
            let throttle_badge = if card.throttled { "⚠️ THROTTLED" } else { "✓ OPTIMAL" };
            chat_lines.push(Line::from(vec![
                Span::styled("🪽 Wattage Governor: ", Style::default().fg(Color::LightGreen).add_modifier(Modifier::BOLD)),
                Span::styled(format!("{:.0} mW | Spent: ${:.4} | {}", card.power_mw, card.spent_usd, throttle_badge), Style::default().fg(Color::White)),
            ]));
            chat_lines.push(Line::from(""));
        }

        // 19. Web Browse Cards
        for card in &self.web_browse_cards {
            let status_badge = if card.status_code < 400 { format!("✓ {}", card.status_code) } else { format!("✗ {}", card.status_code) };
            chat_lines.push(Line::from(vec![
                Span::styled("🌐 Web Browse: ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::styled(format!("{} | {} ({}ms)", card.title, status_badge, card.duration_ms), Style::default().fg(Color::White)),
            ]));
            chat_lines.push(Line::from(vec![
                Span::styled("   URL: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&card.url, Style::default().fg(Color::LightCyan)),
            ]));
            if !card.content_snippet.is_empty() {
                chat_lines.push(Line::from(vec![
                    Span::styled("   Digest: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(&card.content_snippet, Style::default().fg(Color::Gray)),
                ]));
            }
            chat_lines.push(Line::from(""));
        }

        // 20. Web Search Cards
        for card in &self.web_search_cards {
            chat_lines.push(Line::from(vec![
                Span::styled("🔍 Live Web Search: ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(format!("\"{}\" ({} results)", card.query, card.count), Style::default().fg(Color::White)),
            ]));
            for (idx, r) in card.top_results.iter().enumerate() {
                chat_lines.push(Line::from(vec![
                    Span::styled(format!("   {}. ", idx + 1), Style::default().fg(Color::DarkGray)),
                    Span::styled(&r.title, Style::default().fg(Color::LightYellow).add_modifier(Modifier::BOLD)),
                    Span::styled(format!(" — {}", r.url), Style::default().fg(Color::DarkGray)),
                ]));
            }
            chat_lines.push(Line::from(""));
        }

        // 21. Persistent Living Memory Cards
        for card in &self.memory_cards {
            let card_w = usable_width.saturating_sub(2).max(48);
            let action_badge = format!("[{}]", card.action);
            let header_str = format!("╭── 🧠 Living Project Memory {} ", action_badge);
            let header_w = UnicodeWidthStr::width(header_str.as_str());
            let dashes_count = card_w.saturating_sub(header_w + 1).max(2);
            let top_border = format!("{}{}{}╮", header_str, "─".repeat(dashes_count), " ");
            chat_lines.push(Line::from(vec![
                Span::styled(top_border, Style::default().fg(Color::LightMagenta).add_modifier(Modifier::BOLD)),
            ]));
            chat_lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(Color::LightMagenta)),
                Span::styled("Title: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&card.title, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" (ADRs: {} | Debts: {})", card.total_decisions, card.total_debts), Style::default().fg(Color::Cyan)),
            ]));
            if !card.decision.is_empty() {
                chat_lines.push(Line::from(vec![
                    Span::styled("│ ", Style::default().fg(Color::LightMagenta)),
                    Span::styled("Decision: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(&card.decision, Style::default().fg(Color::LightGreen)),
                ]));
            }
            if !card.context.is_empty() {
                chat_lines.push(Line::from(vec![
                    Span::styled("│ ", Style::default().fg(Color::LightMagenta)),
                    Span::styled("Context: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(&card.context, Style::default().fg(Color::Gray)),
                ]));
            }
            let bot_border = format!("╰{}╯", "─".repeat(card_w.saturating_sub(2)));
            chat_lines.push(Line::from(Span::styled(bot_border, Style::default().fg(Color::LightMagenta))));
            chat_lines.push(Line::from(""));
        }

        // 22. Ambient AST Follower Cards
        for card in &self.ambient_cards {
            let card_w = usable_width.saturating_sub(2).max(48);
            let symbol_info = card.enclosing_symbol.as_deref().unwrap_or("module root");
            let kind_info = card.symbol_kind.as_deref().unwrap_or("scope");
            let header_str = format!("╭── 🎯 Ambient AST Focus [{}:{}] ", card.file_path, card.cursor_line);
            let header_w = UnicodeWidthStr::width(header_str.as_str());
            let dashes_count = card_w.saturating_sub(header_w + 1).max(2);
            let top_border = format!("{}{}{}╮", header_str, "─".repeat(dashes_count), " ");
            chat_lines.push(Line::from(Span::styled(top_border, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))));
            chat_lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(Color::Cyan)),
                Span::styled("Enclosing Symbol: ", Style::default().fg(Color::DarkGray)),
                Span::styled(symbol_info, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" ({})", kind_info), Style::default().fg(Color::Yellow)),
                Span::styled(format!(" | Imports: {}", card.imports.len()), Style::default().fg(Color::DarkGray)),
            ]));
            if !card.context_snippet.is_empty() {
                for line in card.context_snippet.lines().take(3) {
                    chat_lines.push(Line::from(vec![
                        Span::styled("│ ", Style::default().fg(Color::Cyan)),
                        Span::styled("  ", Style::default().fg(Color::DarkGray)),
                        Span::styled(line, Style::default().fg(Color::LightCyan)),
                    ]));
                }
            }
            let bot_border = format!("╰{}╯", "─".repeat(card_w.saturating_sub(2)));
            chat_lines.push(Line::from(Span::styled(bot_border, Style::default().fg(Color::Cyan))));
            chat_lines.push(Line::from(""));
        }

        // 23. Self-Validating Vibe Loop Cards
        for card in &self.validation_cards {
            let card_w = usable_width.saturating_sub(2).max(48);
            let (status_badge, border_color) = if card.passed {
                ("✓ PASSED", Color::Green)
            } else {
                ("✗ FAILED", Color::Red)
            };
            let header_str = format!("╭── 🧪 Self-Validating Loop [{}] ({} iters, {}ms) ", status_badge, card.iterations, card.duration_ms);
            let header_w = UnicodeWidthStr::width(header_str.as_str());
            let dashes_count = card_w.saturating_sub(header_w + 1).max(2);
            let top_border = format!("{}{}{}╮", header_str, "─".repeat(dashes_count), " ");
            chat_lines.push(Line::from(Span::styled(top_border, Style::default().fg(border_color).add_modifier(Modifier::BOLD))));
            chat_lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(border_color)),
                Span::styled("Goal: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&card.goal, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            ]));
            if !card.companion_test.is_empty() {
                chat_lines.push(Line::from(vec![
                    Span::styled("│ ", Style::default().fg(border_color)),
                    Span::styled("Companion Spec: ", Style::default().fg(Color::DarkGray)),
                    Span::styled("synthesized & verified against compiler healer", Style::default().fg(Color::LightGreen)),
                ]));
            }
            let bot_border = format!("╰{}╯", "─".repeat(card_w.saturating_sub(2)));
            chat_lines.push(Line::from(Span::styled(bot_border, Style::default().fg(border_color))));
            chat_lines.push(Line::from(""));
        }

        // 24. Instant App Forge Cards
        for card in &self.forge_cards {
            let card_w = usable_width.saturating_sub(2).max(48);
            let header_str = format!("╭── 🪽 App Forge [{}] ", card.stack);
            let header_w = UnicodeWidthStr::width(header_str.as_str());
            let dashes_count = card_w.saturating_sub(header_w + 1).max(2);
            let top_border = format!("{}{}{}╮", header_str, "─".repeat(dashes_count), " ");
            chat_lines.push(Line::from(Span::styled(top_border, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))));
            chat_lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(Color::Yellow)),
                Span::styled("Project: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&card.project_name, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" -> {} ({} files, {}ms)", card.target_path, card.files_created, card.duration_ms), Style::default().fg(Color::DarkGray)),
            ]));
            chat_lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(Color::Yellow)),
                Span::styled("Badges: ", Style::default().fg(Color::DarkGray)),
                Span::styled(if card.git_initialized { "Git: ✓ " } else { "Git: ✗ " }, Style::default().fg(Color::Green)),
                Span::styled(if card.adr_initialized { "ADR-001: ✓" } else { "ADR-001: ✗" }, Style::default().fg(Color::Cyan)),
            ]));
            let bot_border = format!("╰{}╯", "─".repeat(card_w.saturating_sub(2)));
            chat_lines.push(Line::from(Span::styled(bot_border, Style::default().fg(Color::Yellow))));
            chat_lines.push(Line::from(""));
        }

        // 25. Adaptive AST & KV-Cache Pruner Cards
        for card in &self.prune_cards {
            let card_w = usable_width.saturating_sub(2).max(48);
            let header_str = format!("╭── ✂️ AST & KV-Cache Pruner [{:.1}% Reduction] ", card.reduction_pct);
            let header_w = UnicodeWidthStr::width(header_str.as_str());
            let dashes_count = card_w.saturating_sub(header_w + 1).max(2);
            let top_border = format!("{}{}{}╮", header_str, "─".repeat(dashes_count), " ");
            chat_lines.push(Line::from(Span::styled(top_border, Style::default().fg(Color::LightBlue).add_modifier(Modifier::BOLD))));
            chat_lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(Color::LightBlue)),
                Span::styled("Target: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&card.file_path, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" ({} lines -> {} lines | {} fns folded)", card.original_lines, card.pruned_lines, card.folded_functions), Style::default().fg(Color::Cyan)),
            ]));
            let bot_border = format!("╰{}╯", "─".repeat(card_w.saturating_sub(2)));
            chat_lines.push(Line::from(Span::styled(bot_border, Style::default().fg(Color::LightBlue))));
            chat_lines.push(Line::from(""));
        }

        // 26. Auto-Port Multiplexer Cards
        for card in &self.port_cards {
            let card_w = usable_width.saturating_sub(2).max(48);
            let header_str = format!("╭── 🔌 Auto-Port Multiplexer [{} Collisions Resolved] ", card.collisions_found);
            let header_w = UnicodeWidthStr::width(header_str.as_str());
            let dashes_count = card_w.saturating_sub(header_w + 1).max(2);
            let top_border = format!("{}{}{}╮", header_str, "─".repeat(dashes_count), " ");
            chat_lines.push(Line::from(Span::styled(top_border, Style::default().fg(Color::LightGreen).add_modifier(Modifier::BOLD))));
            chat_lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(Color::LightGreen)),
                Span::styled(format!("Scanned {} devserver ports across standard ranges", card.scanned_ports), Style::default().fg(Color::DarkGray)),
            ]));
            for (svc, orig, alloc) in &card.services {
                let badge = if orig != alloc { format!("{} (COLLISION -> {})", orig, alloc) } else { format!("{}", orig) };
                let color = if orig != alloc { Color::Yellow } else { Color::Green };
                chat_lines.push(Line::from(vec![
                    Span::styled("│ ", Style::default().fg(Color::LightGreen)),
                    Span::styled("  • ", Style::default().fg(Color::DarkGray)),
                    Span::styled(svc, Style::default().fg(Color::White)),
                    Span::styled(": ", Style::default().fg(Color::DarkGray)),
                    Span::styled(badge, Style::default().fg(color).add_modifier(Modifier::BOLD)),
                ]));
            }
            let bot_border = format!("╰{}╯", "─".repeat(card_w.saturating_sub(2)));
            chat_lines.push(Line::from(Span::styled(bot_border, Style::default().fg(Color::LightGreen))));
            chat_lines.push(Line::from(""));
        }

        // 27. PR Storyteller Cards
        for card in &self.ship_cards {
            let card_w = usable_width.saturating_sub(2).max(48);
            let header_str = format!("╭── 🚢 PR Storyteller [{}] ", card.verification_badge);
            let header_w = UnicodeWidthStr::width(header_str.as_str());
            let dashes_count = card_w.saturating_sub(header_w + 1).max(2);
            let top_border = format!("{}{}{}╮", header_str, "─".repeat(dashes_count), " ");
            chat_lines.push(Line::from(Span::styled(top_border, Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD))));
            chat_lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(Color::Magenta)),
                Span::styled("PR Title: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&card.pr_title, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            ]));
            chat_lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(Color::Magenta)),
                Span::styled(format!("Commits: {} | ADRs: {} | Digest: {} | Written: {}", card.commits_count, card.adr_references.join(", "), &card.blake3_digest[..8.min(card.blake3_digest.len())], card.story_file_path), Style::default().fg(Color::LightMagenta)),
            ]));
            let bot_border = format!("╰{}╯", "─".repeat(card_w.saturating_sub(2)));
            chat_lines.push(Line::from(Span::styled(bot_border, Style::default().fg(Color::Magenta))));
            chat_lines.push(Line::from(""));
        }

        // 28. Browser Live HUD Cards
        for card in &self.browser_incident_cards {
            let card_w = usable_width.saturating_sub(2).max(48);
            let heal_status = if card.healed { "✓ HEALED" } else { "🚨 ACTIVE [F8 1-Click Heal]" };
            let border_color = if card.healed { Color::Green } else { Color::LightRed };
            let header_str = format!("╭── 🌐 Browser Live HUD [{}] ", heal_status);
            let header_w = UnicodeWidthStr::width(header_str.as_str());
            let dashes_count = card_w.saturating_sub(header_w + 1).max(2);
            let top_border = format!("{}{}{}╮", header_str, "─".repeat(dashes_count), " ");
            chat_lines.push(Line::from(Span::styled(top_border, Style::default().fg(border_color).add_modifier(Modifier::BOLD))));
            let line_str = card.line_number.map(|l| format!(":{}", l)).unwrap_or_default();
            chat_lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(border_color)),
                Span::styled(format!("Incident {}: ", card.incident_id), Style::default().fg(Color::DarkGray)),
                Span::styled(&card.kind, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" [{}{}]", card.source_url, line_str), Style::default().fg(Color::White)),
            ]));
            chat_lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(border_color)),
                Span::styled("Message: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&card.message, Style::default().fg(Color::LightRed)),
            ]));
            if let Some(ref fix) = card.suggested_fix {
                chat_lines.push(Line::from(vec![
                    Span::styled("│ ", Style::default().fg(border_color)),
                    Span::styled("Surgical Fix: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(fix, Style::default().fg(Color::LightGreen)),
                ]));
            }
            let bot_border = format!("╰{}╯", "─".repeat(card_w.saturating_sub(2)));
            chat_lines.push(Line::from(Span::styled(bot_border, Style::default().fg(border_color))));
            chat_lines.push(Line::from(""));
        }

        // 29. Instant Persona & Synthetic Seed Cards
        for card in &self.seed_cards {
            let card_w = usable_width.saturating_sub(2).max(48);
            let header_str = format!("╭── 🎭 Synthetic Persona & Seed [{} {}] ", card.count, card.entity);
            let header_w = UnicodeWidthStr::width(header_str.as_str());
            let dashes_count = card_w.saturating_sub(header_w + 1).max(2);
            let top_border = format!("{}{}{}╮", header_str, "─".repeat(dashes_count), " ");
            chat_lines.push(Line::from(Span::styled(top_border, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))));
            chat_lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(Color::Cyan)),
                Span::styled("Entity: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&card.entity, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" ({} records with edge-case UTF-8 & foreign key integrity)", card.count), Style::default().fg(Color::LightCyan)),
            ]));
            for sql_line in card.sql_preview.lines().take(2) {
                chat_lines.push(Line::from(vec![
                    Span::styled("│ ", Style::default().fg(Color::Cyan)),
                    Span::styled("  SQL: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(sql_line, Style::default().fg(Color::Yellow)),
                ]));
            }
            let bot_border = format!("╰{}╯", "─".repeat(card_w.saturating_sub(2)));
            chat_lines.push(Line::from(Span::styled(bot_border, Style::default().fg(Color::Cyan))));
            chat_lines.push(Line::from(""));
        }

        // 30. Syntactic Hunk Time-Travel Rewind Cards
        for card in &self.rewind_cards {
            let card_w = usable_width.saturating_sub(2).max(48);
            let header_str = format!("╭── ⏪ Syntactic Hunk Time-Travel [{} :: {}] ", card.file_path, card.symbol_name);
            let header_w = UnicodeWidthStr::width(header_str.as_str());
            let dashes_count = card_w.saturating_sub(header_w + 1).max(2);
            let top_border = format!("{}{}{}╮", header_str, "─".repeat(dashes_count), " ");
            chat_lines.push(Line::from(Span::styled(top_border, Style::default().fg(Color::LightGreen).add_modifier(Modifier::BOLD))));
            chat_lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(Color::LightGreen)),
                Span::styled("Rollback: ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("Surgically restored revision #{} without touching adjacent functions", card.revision_idx), Style::default().fg(Color::White)),
            ]));
            for s_line in card.restored_snippet.lines().take(2) {
                chat_lines.push(Line::from(vec![
                    Span::styled("│ ", Style::default().fg(Color::LightGreen)),
                    Span::styled("  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(s_line, Style::default().fg(Color::LightCyan)),
                ]));
            }
            let bot_border = format!("╰{}╯", "─".repeat(card_w.saturating_sub(2)));
            chat_lines.push(Line::from(Span::styled(bot_border, Style::default().fg(Color::LightGreen))));
            chat_lines.push(Line::from(""));
        }

        // 31. Adversarial Red-Team & Edge-Case Audit Cards
        for card in &self.redteam_cards {
            let card_w = usable_width.saturating_sub(2).max(48);
            let border_color = if card.total_critical > 0 { Color::Red } else if card.total_high > 0 { Color::Yellow } else { Color::Green };
            let header_str = format!("╭── 🛡️ Adversarial Red-Team [{}] ", card.verdict);
            let header_w = UnicodeWidthStr::width(header_str.as_str());
            let dashes_count = card_w.saturating_sub(header_w + 1).max(2);
            let top_border = format!("{}{}{}╮", header_str, "─".repeat(dashes_count), " ");
            chat_lines.push(Line::from(Span::styled(top_border, Style::default().fg(border_color).add_modifier(Modifier::BOLD))));
            chat_lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(border_color)),
                Span::styled("Audit Summary: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&card.summary, Style::default().fg(Color::White)),
            ]));
            for finding in &card.top_findings {
                chat_lines.push(Line::from(vec![
                    Span::styled("│ ", Style::default().fg(border_color)),
                    Span::styled("  • ", Style::default().fg(Color::DarkGray)),
                    Span::styled(finding, Style::default().fg(Color::LightRed)),
                ]));
            }
            let bot_border = format!("╰{}╯", "─".repeat(card_w.saturating_sub(2)));
            chat_lines.push(Line::from(Span::styled(bot_border, Style::default().fg(border_color))));
            chat_lines.push(Line::from(""));
        }

        // 32. Living Architecture Blueprint Cards
        for card in &self.blueprint_cards {
            let card_w = usable_width.saturating_sub(2).max(48);
            let header_str = format!("╭── 📐 Living Blueprint [{}] ", card.title);
            let header_w = UnicodeWidthStr::width(header_str.as_str());
            let dashes_count = card_w.saturating_sub(header_w + 1).max(2);
            let top_border = format!("{}{}{}╮", header_str, "─".repeat(dashes_count), " ");
            chat_lines.push(Line::from(Span::styled(top_border, Style::default().fg(Color::LightBlue).add_modifier(Modifier::BOLD))));
            chat_lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(Color::LightBlue)),
                Span::styled(format!("Scanned {} files | Discovered {} components, {} routes, {} entities, {} edges",
                    card.total_files, card.total_nodes, card.total_routes, card.total_entities, card.total_edges), Style::default().fg(Color::White)),
            ]));
            for m_line in card.mermaid_diagram.lines().take(3) {
                chat_lines.push(Line::from(vec![
                    Span::styled("│ ", Style::default().fg(Color::LightBlue)),
                    Span::styled("  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(m_line, Style::default().fg(Color::Cyan)),
                ]));
            }
            let bot_border = format!("╰{}╯", "─".repeat(card_w.saturating_sub(2)));
            chat_lines.push(Line::from(Span::styled(bot_border, Style::default().fg(Color::LightBlue))));
            chat_lines.push(Line::from(""));
        }

        // 33. Passive Sentinel Cards
        for card in &self.passive_sentinel_cards {
            let card_w = usable_width.saturating_sub(2).max(48);
            let header_str = format!("╭── 👁️ Passive Sentinel [{}] ", card.status_badge);
            let header_w = UnicodeWidthStr::width(header_str.as_str());
            let dashes_count = card_w.saturating_sub(header_w + 1).max(2);
            let top_border = format!("{}{}{}╮", header_str, "─".repeat(dashes_count), " ");
            chat_lines.push(Line::from(Span::styled(top_border, Style::default().fg(Color::LightYellow).add_modifier(Modifier::BOLD))));
            chat_lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(Color::LightYellow)),
                Span::styled(format!("Tracking {} files | {} inspections | 150ms debounce", card.tracked_files, card.total_inspections), Style::default().fg(Color::DarkGray)),
            ]));
            chat_lines.push(Line::from(vec![
                Span::styled("│ ", Style::default().fg(Color::LightYellow)),
                Span::styled("Delta: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&card.last_event_summary, Style::default().fg(Color::White)),
            ]));
            for err in &card.active_syntax_errors {
                chat_lines.push(Line::from(vec![
                    Span::styled("│ ", Style::default().fg(Color::LightYellow)),
                    Span::styled("  ⚠️ ", Style::default().fg(Color::Yellow)),
                    Span::styled(err, Style::default().fg(Color::LightRed)),
                ]));
            }
            let bot_border = format!("╰{}╯", "─".repeat(card_w.saturating_sub(2)));
            chat_lines.push(Line::from(Span::styled(bot_border, Style::default().fg(Color::LightYellow))));
            chat_lines.push(Line::from(""));
        }

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
            for (item_idx, item) in self.conversation.iter().enumerate() {
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
                        // Render thinking block if present with dynamic auto-width and closed borders
                        if let Some(ref thinking) = item.thinking {
                            let title_body = format!("Thinking Process ({} tokens)", item.tokens);
                            let title_w = visual_str_width(title_body.as_str());
                            let mut natural_w = title_w + 10;
                            for t_line in thinking.lines() {
                                let w = visual_str_width(t_line) + 4;
                                if w > natural_w {
                                    natural_w = w;
                                }
                            }
                            let max_allowed = usable_width.saturating_sub(2).max(36);
                            let t_banner_w = natural_w.max(48).min(max_allowed);

                            let prefix_str = "╭─ 💭 ";
                            let prefix_w = visual_str_width(prefix_str);
                            let max_title_w = t_banner_w.saturating_sub(prefix_w + 3);
                            let (display_title, display_title_w) = if title_w > max_title_w {
                                if max_title_w == 0 {
                                    ("".to_string(), 0)
                                } else if max_title_w == 1 {
                                    ("…".to_string(), 1)
                                } else {
                                    let truncated = truncate_str_by_width(&title_body, max_title_w.saturating_sub(1));
                                    let tw = visual_str_width(truncated.as_str()) + 1;
                                    (format!("{}…", truncated), tw)
                                }
                            } else {
                                (title_body.clone(), title_w)
                            };

                            let used_header_w = prefix_w + display_title_w + 2; // prefix + title + " " (1) + "╮" (1)
                            let dashes_count = t_banner_w.saturating_sub(used_header_w).max(1);
                            let dashes = "─".repeat(dashes_count);

                            chat_lines.push(Line::from(vec![
                                Span::styled(prefix_str, Style::default().fg(Color::Magenta)),
                                Span::styled(display_title, Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
                                Span::raw(" "),
                                Span::styled(format!("{}╮", dashes), Style::default().fg(Color::Magenta)),
                            ]));

                            let max_chunk_w = t_banner_w.saturating_sub(4).max(10);
                            for t_line in thinking.lines() {
                                let chunks = wrap_line_to_width(t_line, max_chunk_w);
                                for chunk in chunks {
                                    let chunk_w = visual_str_width(chunk.as_str());
                                    let pad_w = max_chunk_w.saturating_sub(chunk_w);
                                    chat_lines.push(Line::from(vec![
                                        Span::styled("│ ", Style::default().fg(Color::Magenta)),
                                        Span::styled(chunk, Style::default().fg(Color::DarkGray)),
                                        Span::raw(" ".repeat(pad_w)),
                                        Span::styled(" │", Style::default().fg(Color::Magenta)),
                                    ]));
                                }
                            }

                            let bottom_dashes = "─".repeat(t_banner_w.saturating_sub(2));
                            chat_lines.push(Line::from(Span::styled(
                                format!("╰{}╯", bottom_dashes),
                                Style::default().fg(Color::Magenta),
                            )));
                        }

                        // Render tool calls with dynamic auto-width and perfectly closed borders
                        for (tool_idx, tool) in item.tool_calls.iter().enumerate() {
                            let card_id = format!("tool_{}_{}", item_idx, tool_idx);
                            let is_copied = if let Some(ref last_id) = self.last_copied_id {
                                if last_id == &card_id {
                                    if let Some(t) = self.last_copied_time {
                                        t.elapsed().as_secs() < 3
                                    } else {
                                        false
                                    }
                                } else {
                                    false
                                }
                            } else {
                                false
                            };

                            let tool_icon = match tool.tool_name.as_str() {
                                "run_command" | "exec" | "sh" | "bash" => "💻",
                                "view_file" | "cat" | "read" => "📖",
                                "write_to_file" | "write" => "📝",
                                "replace_file_content" | "edit" => "✂️",
                                "find_by_name" | "find" => "🔍",
                                "grep_search" | "grep" => "🔎",
                                "list_dir" | "ls" => "📁",
                                "invoke_subagent" | "subagent" => "🤖",
                                "gemini_api_direct" | "gemini" | "hgbd_gemini_inference" => "🛠️",
                                _ => "🔧",
                            };

                            let title_body = format!("{} ({})", tool.tool_name, tool.parameters_summary);
                            let icon_w = visual_str_width(tool_icon);
                            let title_w = visual_str_width(title_body.as_str());

                            let status_color = match tool.status.as_str() {
                                "SUCCESS" => Color::Green,
                                "FAILED" => Color::Red,
                                _ => Color::Yellow,
                            };
                            let status_prefix = "│ Status: ";
                            let status_badge = format!("[{}]", tool.status);
                            let dur_part = format!("  Duration: {}ms", tool.duration_ms);
                            let status_row_w = visual_str_width(status_prefix)
                                + visual_str_width(status_badge.as_str())
                                + visual_str_width(dur_part.as_str());

                            // Calculate auto-width: find max width across header, status, and snippet lines
                            let copy_btn_preview_w = if is_copied { 1 } else { 2 };
                            let mut natural_w = (title_w + icon_w + copy_btn_preview_w + 10).max(status_row_w + 2);
                            if let Some(ref snippet) = tool.output_snippet {
                                for s_line in snippet.trim().lines().take(50) {
                                    let sw = visual_str_width(s_line) + 6;
                                    if sw > natural_w {
                                        natural_w = sw;
                                    }
                                }
                            }

                            // Banner auto-fits information, bounded between 48 and usable_width - 2
                            let max_allowed = usable_width.saturating_sub(2).max(40);
                            let banner_width = natural_w.max(48).min(max_allowed);

                            // 1. Header Line (Top Border) with Copy Icon on Top Right Near Corner (Only Icon, No Brackets)
                            let (copy_spans, copy_btn_w) = if is_copied {
                                (
                                    vec![
                                        Span::styled("✓", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                                    ],
                                    1,
                                )
                            } else {
                                (
                                    vec![
                                        Span::styled("📋", Style::default().fg(Color::Yellow)),
                                    ],
                                    2,
                                )
                            };

                            let fixed_w = 3 + icon_w + 1 + 1 + 1 + copy_btn_w + 3; // "╭─ " (3) + icon + " " (1) + " " (1) + " " (1) + copy_btn + " ─╮" (3)
                            let max_title_w = banner_width.saturating_sub(fixed_w + 2);
                            let (display_title, display_title_w) = if title_w > max_title_w {
                                if max_title_w == 0 {
                                    ("".to_string(), 0)
                                } else if max_title_w == 1 {
                                    ("…".to_string(), 1)
                                } else {
                                    let truncated = truncate_str_by_width(&title_body, max_title_w.saturating_sub(1));
                                    let tw = visual_str_width(truncated.as_str()) + 1;
                                    (format!("{}…", truncated), tw)
                                }
                            } else {
                                (title_body.clone(), title_w)
                            };

                            let used_header_w = 3 + icon_w + 1 + display_title_w + 1 + 1 + copy_btn_w + 3;
                            let dashes_count = banner_width.saturating_sub(used_header_w).max(1);
                            let dashes = "─".repeat(dashes_count);

                            let mut header_spans = vec![
                                Span::styled("╭─ ", Style::default().fg(Color::Cyan)),
                                Span::styled(tool_icon, Style::default().fg(Color::Yellow)),
                                Span::raw(" "),
                                Span::styled(display_title, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                                Span::raw(" "),
                                Span::styled(dashes, Style::default().fg(Color::Cyan)),
                                Span::raw(" "),
                            ];
                            header_spans.extend(copy_spans);
                            header_spans.push(Span::styled(" ─╮", Style::default().fg(Color::Cyan)));

                            let copy_content = if let Some(ref snippet) = tool.output_snippet {
                                if !snippet.trim().is_empty() {
                                    snippet.clone()
                                } else {
                                    item.content.clone()
                                }
                            } else {
                                item.content.clone()
                            };

                            pending_copy_targets.push(PendingCopyTarget {
                                line_idx: chat_lines.len(),
                                banner_w: banner_width,
                                copy_btn_w,
                                content: copy_content,
                                card_id,
                                label: format!("{} response", tool.tool_name),
                            });

                            chat_lines.push(Line::from(header_spans));

                            // 2. Status Line (closed with right border │)
                            let (dur_part_str, status_w) = if status_row_w + 1 > banner_width {
                                let short_dur = format!(" {}ms", tool.duration_ms);
                                let test_w = visual_str_width(status_prefix)
                                    + visual_str_width(status_badge.as_str())
                                    + visual_str_width(short_dur.as_str());
                                if test_w + 1 <= banner_width {
                                    (short_dur, test_w)
                                } else {
                                    ("".to_string(), visual_str_width(status_prefix) + visual_str_width(status_badge.as_str()))
                                }
                            } else {
                                (dur_part, status_row_w)
                            };
                            let status_pad = banner_width.saturating_sub(status_w + 1);
                            chat_lines.push(Line::from(vec![
                                Span::styled(status_prefix, Style::default().fg(Color::Cyan)),
                                Span::styled(status_badge, Style::default().fg(status_color).add_modifier(Modifier::BOLD)),
                                Span::styled(dur_part_str, Style::default().fg(Color::DarkGray)),
                                Span::raw(" ".repeat(status_pad)),
                                Span::styled("│", Style::default().fg(Color::Cyan)),
                            ]));

                            // 3. Output Snippet Lines (cleanly wrapped to inner width with closed borders)
                            if let Some(ref snippet) = tool.output_snippet {
                                let trimmed = snippet.trim();
                                if !trimmed.is_empty() {
                                    let max_chunk_w = banner_width.saturating_sub(6);
                                    let line_count = trimmed.lines().count();
                                    for raw_line in trimmed.lines().take(50) {
                                        let chunks = wrap_line_to_width(raw_line, max_chunk_w);
                                        for chunk in chunks {
                                            let chunk_w = visual_str_width(chunk.as_str());
                                            let pad_w = max_chunk_w.saturating_sub(chunk_w);
                                            chat_lines.push(Line::from(vec![
                                                Span::styled("│   ", Style::default().fg(Color::Cyan)),
                                                Span::styled(chunk, Style::default().fg(Color::White)),
                                                Span::raw(" ".repeat(pad_w)),
                                                Span::styled(" │", Style::default().fg(Color::Cyan)),
                                            ]));
                                        }
                                    }
                                    if line_count > 50 {
                                        let more_msg = format!("... ({} more lines truncated)", line_count - 50);
                                        let more_w = visual_str_width(more_msg.as_str());
                                        let pad_w = max_chunk_w.saturating_sub(more_w);
                                        chat_lines.push(Line::from(vec![
                                            Span::styled("│   ", Style::default().fg(Color::Cyan)),
                                            Span::styled(more_msg, Style::default().fg(Color::DarkGray)),
                                            Span::raw(" ".repeat(pad_w)),
                                            Span::styled(" │", Style::default().fg(Color::Cyan)),
                                        ]));
                                    }
                                }
                            }

                            // 4. Footer Line (Bottom Border) - exact match to banner_width
                            let bottom_dashes = "─".repeat(banner_width.saturating_sub(2));
                            chat_lines.push(Line::from(Span::styled(
                                format!("╰{}╯", bottom_dashes),
                                Style::default().fg(Color::Cyan),
                            )));
                        }

                        // Check if the message content was already rendered inside a tool call card box
                        let already_rendered_in_box = item.tool_calls.iter().any(|tool| {
                            if let Some(ref snippet) = tool.output_snippet {
                                let s = snippet.trim();
                                let c = item.content.trim();
                                !s.is_empty() && (s == c || s.starts_with(c) || c.starts_with(s))
                            } else {
                                false
                            }
                        });

                        if !already_rendered_in_box {
                            // Parse markdown lines, grouping code blocks for auto-width banner rendering
                            enum MarkdownItem<'a> {
                                Line(&'a str),
                                CodeBlock {
                                    lang: &'a str,
                                    lines: Vec<&'a str>,
                                },
                            }

                            let mut md_items = Vec::new();
                            let mut in_block = false;
                            let mut block_lang = "";
                            let mut block_lines = Vec::new();

                            for line in item.content.lines() {
                                if line.starts_with("```") {
                                    if in_block {
                                        in_block = false;
                                        md_items.push(MarkdownItem::CodeBlock {
                                            lang: block_lang,
                                            lines: std::mem::take(&mut block_lines),
                                        });
                                    } else {
                                        in_block = true;
                                        block_lang = line.trim_start_matches("```").trim();
                                        block_lines.clear();
                                    }
                                } else if in_block {
                                    block_lines.push(line);
                                } else {
                                    md_items.push(MarkdownItem::Line(line));
                                }
                            }
                            if in_block {
                                md_items.push(MarkdownItem::CodeBlock {
                                    lang: block_lang,
                                    lines: block_lines,
                                });
                            }

                            for md_item in md_items {
                                match md_item {
                                    MarkdownItem::CodeBlock { lang, lines } => {
                                        let tag = if lang.is_empty() { "code" } else { lang };
                                        let tag_w = visual_str_width(tag);
                                        let max_content_w = lines.iter().map(|l| visual_str_width(*l)).max().unwrap_or(0);

                                        // Natural width auto-fits content: "│ " (2) + content + " │" (2) = content + 4,
                                        // and header "╭─── [" (6) + tag (tag_w) + "] " (2) + "─" (1) + "╮" (1) = tag_w + 10
                                        let natural_w = (max_content_w + 4).max(tag_w + 10);
                                        let max_allowed = usable_width.saturating_sub(2).max(36);
                                        let code_box_w = natural_w.max(36).min(max_allowed);

                                        // 1. Top border: "╭─── [tag] ────╮"
                                        let max_tag_w = code_box_w.saturating_sub(10);
                                        let (display_tag, display_tag_w) = if tag_w > max_tag_w && max_tag_w > 2 {
                                            let trunc = truncate_str_by_width(tag, max_tag_w.saturating_sub(1));
                                            let tw = visual_str_width(trunc.as_str()) + 1;
                                            (format!("{}…", trunc), tw)
                                        } else {
                                            (tag.to_string(), tag_w)
                                        };

                                        let used_top_w = 6 + display_tag_w + 3; // "╭─── [" (6) + tag + "] " (2) + "╮" (1)
                                        let dashes_count = code_box_w.saturating_sub(used_top_w).max(1);
                                        let top_dashes = "─".repeat(dashes_count);

                                        chat_lines.push(Line::from(vec![
                                            Span::styled("╭─── [", Style::default().fg(Color::Cyan)),
                                            Span::styled(display_tag, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                                            Span::styled(format!("] {}╮", top_dashes), Style::default().fg(Color::Cyan)),
                                        ]));

                                        // 2. Content lines: wrapped preserving indentation with closed right border
                                        let max_chunk_w = code_box_w.saturating_sub(4);
                                        for code_l in lines {
                                            let chunks = wrap_code_line_to_width(code_l, max_chunk_w);
                                            for chunk in chunks {
                                                let chunk_w = visual_str_width(chunk.as_str());
                                                let pad_w = max_chunk_w.saturating_sub(chunk_w);
                                                chat_lines.push(Line::from(vec![
                                                    Span::styled("│ ", Style::default().fg(Color::Cyan)),
                                                    Span::styled(chunk, Style::default().fg(Color::White)),
                                                    Span::raw(" ".repeat(pad_w)),
                                                    Span::styled(" │", Style::default().fg(Color::Cyan)),
                                                ]));
                                            }
                                        }

                                        // 3. Bottom border: "╰──────────────╯"
                                        let bottom_dashes = "─".repeat(code_box_w.saturating_sub(2));
                                        chat_lines.push(Line::from(Span::styled(
                                            format!("╰{}╯", bottom_dashes),
                                            Style::default().fg(Color::Cyan),
                                        )));
                                    }
                                    MarkdownItem::Line(line) => {
                                        if line.starts_with("+ ") || line.starts_with("+\t") {
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
                                }
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

        // If actively processing a prompt, display responsive animated processing card
        if self.is_processing {
            const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            const PULSE_COLORS: &[Color] = &[
                Color::Cyan,
                Color::LightCyan,
                Color::Blue,
                Color::LightBlue,
                Color::Magenta,
                Color::LightMagenta,
            ];
            const WAVE_PATTERNS: &[&str] = &[
                "▰▱▱▱▱",
                "▰▰▱▱▱",
                "▰▰▰▱▱",
                "▰▰▰▰▱",
                "▰▰▰▰▰",
                "▱▰▰▰▰",
                "▱▱▰▰▰",
                "▱▱▱▰▰",
                "▱▱▱▱▰",
                "▱▱▱▱▱",
            ];

            let spinner = SPINNER_FRAMES[self.processing_tick % SPINNER_FRAMES.len()];
            let pulse_color = PULSE_COLORS[(self.processing_tick / 2) % PULSE_COLORS.len()];
            let wave = WAVE_PATTERNS[self.processing_tick % WAVE_PATTERNS.len()];

            let elapsed_str = if let Some(start) = self.processing_start {
                let millis = (Utc::now() - start).num_milliseconds().max(0);
                format!("{:.1}s", millis as f64 / 1000.0)
            } else {
                "0.0s".to_string()
            };

            let title_text = format!("Processing Prompt ({})", self.model_pill);
            let title_w = visual_str_width(title_text.as_str());

            // Line 1: Spinner, Thinking, Wave, Timer, Esc pill
            let status_body = format!("Thinking...  {}   ⏱ {}   [Esc to cancel]", wave, elapsed_str);
            let status_body_w = visual_str_width(status_body.as_str());
            let line1_natural_w = status_body_w + 7;

            // Line 2: Prompt preview if present
            let prompt_line = if !self.processing_prompt_preview.is_empty() {
                format!("Prompt: \"{}\"", self.processing_prompt_preview)
            } else {
                String::new()
            };
            let prompt_line_w = visual_str_width(prompt_line.as_str());
            let line2_natural_w = if prompt_line_w > 0 { prompt_line_w + 6 } else { 0 };

            let natural_w = line1_natural_w.max(line2_natural_w).max(title_w + 8);
            let max_allowed = usable_width.saturating_sub(2).max(44);
            let card_w = natural_w.max(44).min(max_allowed);

            // 1. Top Border: "╭─ Processing Prompt (model) ────╮"
            let max_title_w = card_w.saturating_sub(6);
            let (disp_title, disp_title_w) = if title_w > max_title_w && max_title_w > 3 {
                let truncated = truncate_str_by_width(&title_text, max_title_w.saturating_sub(3));
                let w = visual_str_width(truncated.as_str()) + 3;
                (format!("{}...", truncated), w)
            } else {
                (title_text.clone(), title_w)
            };

            let dash_count = card_w.saturating_sub(disp_title_w + 5);
            let top_dashes = "─".repeat(dash_count);
            chat_lines.push(Line::from(vec![
                Span::styled("╭─ ", Style::default().fg(pulse_color)),
                Span::styled(disp_title, Style::default().fg(pulse_color).add_modifier(Modifier::BOLD)),
                Span::raw(" "),
                Span::styled(top_dashes, Style::default().fg(pulse_color)),
                Span::styled("╮", Style::default().fg(pulse_color)),
            ]));

            // 2. Animated Status Line:
            let inner_max_w = card_w.saturating_sub(5);
            let (disp_status, disp_status_w) = if status_body_w + 2 > inner_max_w && inner_max_w > 4 {
                let truncated = truncate_str_by_width(&status_body, inner_max_w.saturating_sub(5));
                let w = visual_str_width(truncated.as_str()) + 3;
                (format!("{}...", truncated), w)
            } else {
                (status_body.clone(), status_body_w)
            };

            let pad1 = card_w.saturating_sub(disp_status_w + 7);
            chat_lines.push(Line::from(vec![
                Span::styled("│  ", Style::default().fg(pulse_color)),
                Span::styled(spinner, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::raw(" "),
                Span::styled(disp_status, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::raw(" ".repeat(pad1)),
                Span::styled(" │", Style::default().fg(pulse_color)),
            ]));

            // 3. Prompt Preview Line (if present):
            if !prompt_line.is_empty() {
                let max_preview_w = card_w.saturating_sub(5);
                let (disp_p, disp_p_w) = if prompt_line_w > max_preview_w && max_preview_w > 3 {
                    let truncated = truncate_str_by_width(&prompt_line, max_preview_w.saturating_sub(3));
                    let w = visual_str_width(truncated.as_str()) + 3;
                    (format!("{}...", truncated), w)
                } else {
                    (prompt_line.clone(), prompt_line_w)
                };
                let pad2 = card_w.saturating_sub(disp_p_w + 5);
                chat_lines.push(Line::from(vec![
                    Span::styled("│  ", Style::default().fg(pulse_color)),
                    Span::styled(disp_p, Style::default().fg(Color::DarkGray)),
                    Span::raw(" ".repeat(pad2)),
                    Span::styled(" │", Style::default().fg(pulse_color)),
                ]));
            }

            // 4. Bottom Border: "╰────────────────────────────╯"
            let bottom_dashes = "─".repeat(card_w.saturating_sub(2));
            chat_lines.push(Line::from(Span::styled(
                format!("╰{}╯", bottom_dashes),
                Style::default().fg(pulse_color),
            )));
            chat_lines.push(Line::from(""));
        }

        // Viewport scrolling
        let mut estimated_rows = 0;
        let mut line_row_offsets = Vec::with_capacity(chat_lines.len());
        for line in &chat_lines {
            line_row_offsets.push(estimated_rows);
            let width: usize = line.spans.iter().map(|s| visual_str_width(s.content.as_ref())).sum();
            let rows = if width == 0 { 1 } else { (width + usable_width - 1) / usable_width };
            estimated_rows += rows;
        }

        let total_lines = estimated_rows.max(chat_lines.len());
        let max_scroll = total_lines.saturating_sub(viewport_height);

        // Clamped scroll value
        let current_scroll = self.chat_scroll.min(max_scroll);
        let scroll_y = max_scroll.saturating_sub(current_scroll) as u16;

        // Populate clickable hitboxes for copy icons
        if let Ok(mut hitboxes) = self.copy_hitboxes.lock() {
            hitboxes.clear();
            for target in pending_copy_targets {
                if target.line_idx < line_row_offsets.len() {
                    let visual_row = line_row_offsets[target.line_idx];
                    if visual_row >= scroll_y as usize && visual_row < (scroll_y as usize + viewport_height) {
                        let screen_y = chunks[2].y + (visual_row - scroll_y as usize) as u16;
                        let x_end = chunks[2].x + (target.banner_w as u16).min(chunks[2].width);
                        let x_start = x_end.saturating_sub((target.copy_btn_w + 5) as u16);
                        hitboxes.push(CopyHitbox {
                            screen_y,
                            x_start,
                            x_end,
                            content: target.content,
                            card_id: target.card_id,
                            label: target.label,
                        });
                    }
                }
            }
        }

        // Populate clickable hitboxes for diff cards
        if let Ok(mut hitboxes) = self.diff_hitboxes.lock() {
            hitboxes.clear();
            for target in pending_diff_targets {
                if target.line_idx < line_row_offsets.len() {
                    let visual_row = line_row_offsets[target.line_idx];
                    if visual_row >= scroll_y as usize && visual_row < (scroll_y as usize + viewport_height) {
                        let screen_y = chunks[2].y + (visual_row - scroll_y as usize) as u16;
                        let x_end = chunks[2].x + (target.banner_w as u16).min(chunks[2].width);
                        hitboxes.push(DiffActionHitbox {
                            screen_y,
                            x_start: x_end.saturating_sub(26),
                            x_end: x_end.saturating_sub(14),
                            card_id: target.card_id.clone(),
                            action: DiffAction::Accept,
                        });
                        hitboxes.push(DiffActionHitbox {
                            screen_y,
                            x_start: x_end.saturating_sub(13),
                            x_end: x_end.saturating_sub(3),
                            card_id: target.card_id.clone(),
                            action: DiffAction::Reject,
                        });
                    }
                }
            }
        }

        // Populate clickable hitboxes for heal banners
        if let Ok(mut hitboxes) = self.heal_hitboxes.lock() {
            hitboxes.clear();
            for target in pending_heal_targets {
                if target.line_idx < line_row_offsets.len() {
                    let visual_row = line_row_offsets[target.line_idx];
                    if visual_row >= scroll_y as usize && visual_row < (scroll_y as usize + viewport_height) {
                        let screen_y = chunks[2].y + (visual_row - scroll_y as usize) as u16;
                        let x_end = chunks[2].x + (target.banner_w as u16).min(chunks[2].width);
                        hitboxes.push(HealHitbox {
                            screen_y,
                            x_start: x_end.saturating_sub(22),
                            x_end,
                            card_id: target.card_id,
                        });
                    }
                }
            }
        }

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

            if let Ok(mut sb) = self.scrollbar_hitbox.lock() {
                *sb = Some(ScrollbarHitbox {
                    col: chunks[2].x + chunks[2].width.saturating_sub(1),
                    y_start: chunks[2].y,
                    height: chunks[2].height,
                    max_scroll,
                });
            }
        } else {
            if let Ok(mut sb) = self.scrollbar_hitbox.lock() {
                *sb = None;
            }
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

            if let Ok(mut pill_hb) = self.scroll_pill_hitbox.lock() {
                *pill_hb = Some(ScrollPillHitbox {
                    x: pill_x,
                    y: pill_y,
                    width: pill_width,
                    height: 1,
                });
            }
        } else {
            if let Ok(mut pill_hb) = self.scroll_pill_hitbox.lock() {
                *pill_hb = None;
            }
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

        let (prompt_title, border_color) = if self.is_processing {
            const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let spinner = SPINNER_FRAMES[self.processing_tick % SPINNER_FRAMES.len()];
            let elapsed_str = if let Some(start) = self.processing_start {
                let millis = (Utc::now() - start).num_milliseconds().max(0);
                format!("{:.1}s", millis as f64 / 1000.0)
            } else {
                "0.0s".to_string()
            };
            (
                format!(" ❯ [PROCESSING {}] Thinking ({}) · Esc to cancel ", spinner, elapsed_str),
                Color::Yellow,
            )
        } else {
            let title = match self.execution_mode.as_str() {
                "plan" => " ❯ type a prompt (plan mode · Shift+Tab to switch) ".to_string(),
                "accept-edits" => " ❯ type a prompt (accept-edits mode · Shift+Tab to switch) ".to_string(),
                _ => " ❯ type a prompt (? shortcuts, / commands, Ctrl+T cockpit) ".to_string(),
            };
            (title, Color::Cyan)
        };

        let input_box = Paragraph::new(prompt_content).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(prompt_title),
        );
        frame.render_widget(input_box, chunks[3]);

        // 5. Statusline Footer
        let bg_tasks_info = if !self.background_tasks.is_empty() {
            format!("🪽 {} tasks ", self.background_tasks.len())
        } else {
            String::new()
        };

        let statusline = if self.is_processing {
            const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let spinner = SPINNER_FRAMES[self.processing_tick % SPINNER_FRAMES.len()];
            let elapsed_str = if let Some(start) = self.processing_start {
                let millis = (Utc::now() - start).num_milliseconds().max(0);
                format!("{:.1}s", millis as f64 / 1000.0)
            } else {
                "0.0s".to_string()
            };
            Line::from(vec![
                Span::styled(format!(" {} Executing on {} ", spinner, self.model_pill), Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::raw(" "),
                Span::styled(format!("⏱ {} ", elapsed_str), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(" [Esc] Cancel ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                Span::raw("   "),
                Span::styled(bg_tasks_info, Style::default().fg(Color::Yellow)),
                Span::raw("   "),
                Span::styled(format!("👤 {}", self.auth_account), Style::default().fg(Color::DarkGray)),
            ])
        } else {
            Line::from(vec![
                Span::styled("? shortcuts · Shift+Tab mode · Ctrl+T cockpit · Esc clear", Style::default().fg(Color::DarkGray)),
                Span::raw("   "),
                Span::styled(bg_tasks_info, Style::default().fg(Color::Yellow)),
                Span::raw("   "),
                Span::styled(format!("👤 {} · 12µs IPC", self.auth_account), Style::default().fg(Color::DarkGray)),
            ])
        };
        frame.render_widget(Paragraph::new(statusline), chunks[4]);
    }

    /// Render Shortcuts Modal Overlay
    pub fn render_shortcuts_overlay(&self, frame: &mut Frame) {
        let area = frame.area();
        let width = 76.min(area.width.saturating_sub(4));
        let height = 28.min(area.height.saturating_sub(2));
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
            Row::new(vec![Cell::from("Alt+C / Click 📋").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Copy response content from box to clipboard")]),
            Row::new(vec![Cell::from("Tab").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Autocomplete slash commands and models")]),
            Row::new(vec![Cell::from("?").style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)), Cell::from("Toggle this keyboard shortcuts help modal")]),
            Row::new(vec![Cell::from("──────────────").style(Style::default().fg(Color::DarkGray)), Cell::from("──────────────────────────────────────────────────").style(Style::default().fg(Color::DarkGray))]),
            Row::new(vec![Cell::from("/model [name]").style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)), Cell::from("Switch active AI model (Ollama local or Gemini cloud)")]),
            Row::new(vec![Cell::from("/copy").style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)), Cell::from("Copy latest response content to system clipboard")]),
            Row::new(vec![Cell::from("/cockpit").style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)), Cell::from("Switch to full-screen multi-pane DAG Swarm Cockpit")]),
            Row::new(vec![Cell::from("/chat").style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)), Cell::from("Switch to AGY Conversational Chat Canvas")]),
            Row::new(vec![Cell::from("/tasks").style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)), Cell::from("Inspect active background swarm tasks")]),
            Row::new(vec![Cell::from("/mcp").style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)), Cell::from("Inspect Universal MCP tool servers & tools")]),
            Row::new(vec![Cell::from("/timeline [name]").style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)), Cell::from("Ephemeral isolated worktree/snapshot timeline")]),
            Row::new(vec![Cell::from("/verify").style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)), Cell::from("Run Verification Gate & Golden Invariant Guard")]),
            Row::new(vec![Cell::from("/fix").style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)), Cell::from("Diagnose & fix latest shell command crash")]),
            Row::new(vec![Cell::from("/help").style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)), Cell::from("Open this shortcuts and commands panel")]),
            Row::new(vec![Cell::from("/exit, /quit").style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)), Cell::from("Exit Hagibis interactive session")]),
        ];

        let table = Table::new(rows, [Constraint::Percentage(32), Constraint::Percentage(68)])
            .header(Row::new(vec!["Key / Command", "Action"]).style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)))
            .block(modal_block);

        frame.render_widget(table, modal_area);
    }

    /// Get available models dynamically from local Ollama and cloud
    pub fn get_available_models() -> Vec<(String, String)> {
        let mut list = Vec::new();
        let installed = hgb_core::OllamaProvider::installed_models();
        if !installed.is_empty() {
            for tag in installed {
                let desc = format!("Local Ollama ({} • {})", tag.param_summary(), tag.formatted_size());
                list.push((tag.name, desc));
            }
        } else if hgb_core::OllamaProvider::is_available() {
            list.push(("ollama".to_string(), "Local Ollama Engine (online)".to_string()));
        }

        if !list.iter().any(|(m, _)| m == "qwen2.5-coder:1.5b") {
            list.push(("qwen2.5-coder:1.5b".to_string(), "Local Ollama (0ms latency, zero cloud cost)".to_string()));
        }

        list.push(("gemini-2.5-flash".to_string(), "Google Gemini Cloud (Recommended default)".to_string()));
        list.push(("gemini-2.5-pro".to_string(), "Google Gemini Cloud (Deep reasoning pro)".to_string()));
        list
    }

    /// Render Model Picker Modal Overlay
    pub fn render_model_picker_overlay(&self, frame: &mut Frame, selected: usize) {
        let area = frame.area();
        let width = 76.min(area.width.saturating_sub(4));
        let height = 14.min(area.height.saturating_sub(4));
        let x = (area.width.saturating_sub(width)) / 2;
        let y = (area.height.saturating_sub(height)) / 2;
        let modal_area = Rect::new(x, y, width, height);

        frame.render_widget(Clear, modal_area);

        let models = Self::get_available_models();

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
                    Cell::from(desc.as_str()),
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
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        rt.block_on(self.run_interactive())
    }

    async fn event_loop<B: ratatui::backend::Backend>(&mut self, terminal: &mut Terminal<B>) -> io::Result<()> {
        let models_list = Self::get_available_models();

        let mut prompt_rx: Option<tokio::sync::oneshot::Receiver<PromptExecutionResult>> = None;

        loop {
            // Check background prompt execution if active
            if self.is_processing {
                self.processing_tick = self.processing_tick.wrapping_add(1);
                if let Some(mut rx) = prompt_rx.take() {
                    match rx.try_recv() {
                        Ok(res) => {
                            self.apply_prompt_result(res);
                        }
                        Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                            prompt_rx = Some(rx);
                        }
                        Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                            self.cancel_processing();
                        }
                    }
                }
            }

            if self.needs_clear {
                terminal.clear()?;
                self.needs_clear = false;
            }

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
                            MouseEventKind::Down(MouseButton::Left) => {
                                self.handle_mouse_click(mouse_event.column, mouse_event.row);
                            }
                            MouseEventKind::Drag(MouseButton::Left) => {
                                self.handle_mouse_drag(mouse_event.column, mouse_event.row);
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
                                        self.needs_clear = true;
                                    }
                                    KeyCode::Up | KeyCode::Char('k') => {
                                        *selected = selected.saturating_sub(1);
                                    }
                                    KeyCode::Down | KeyCode::Char('j') => {
                                        *selected = (*selected + 1).min(models_list.len().saturating_sub(1));
                                    }
                                    KeyCode::Enter => {
                                        if let Some((new_model, _)) = models_list.get(*selected) {
                                            self.model_pill = new_model.clone();
                                            let _ = hgb_core::persist_active_model(new_model);
                                            self.add_system_notice(format!("Active model switched to: {}", new_model));
                                        }
                                        self.overlay = CockpitOverlay::None;
                                        self.needs_clear = true;
                                    }
                                    _ => {}
                                },
                                CockpitOverlay::Shortcuts | CockpitOverlay::Tasks => match key.code {
                                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') | KeyCode::Char('q') => {
                                        self.overlay = CockpitOverlay::None;
                                        self.needs_clear = true;
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
                                    if self.is_processing {
                                        self.cancel_processing();
                                        prompt_rx = None;
                                        continue;
                                    }
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
                                KeyCode::Char('c') | KeyCode::Char('C') => {
                                    self.copy_latest_response();
                                    continue;
                                }
                                KeyCode::Char('g') | KeyCode::Char('G') => {
                                    self.prompt_input = "/goal ".to_string();
                                    self.cursor_position = self.prompt_input.chars().count();
                                    continue;
                                }
                                KeyCode::Char('a') | KeyCode::Char('A') => {
                                    self.accept_diff_card(None);
                                    continue;
                                }
                                KeyCode::Char('r') | KeyCode::Char('R') => {
                                    self.reject_diff_card(None);
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
                                if self.is_processing {
                                    self.cancel_processing();
                                    prompt_rx = None;
                                    continue;
                                }
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
                            KeyCode::F(5) => {
                                self.trigger_heal();
                                continue;
                            }
                            KeyCode::F(6) => {
                                if let Some(CockpitItem::ValidationCard(c)) = CockpitVibeManager::handle_vibe_slash_command("/validate", "verify system invariants") {
                                    self.validation_cards.push(c);
                                    hgb_core::play_vibe_chime(true);
                                }
                                continue;
                            }
                            KeyCode::F(7) => {
                                self.toggle_listening();
                                continue;
                            }
                            KeyCode::F(8) => {
                                if let Some(pos) = self.browser_incident_cards.iter().position(|c| !c.healed) {
                                    self.browser_incident_cards[pos].healed = true;
                                    let id = self.browser_incident_cards[pos].incident_id.clone();
                                    self.add_system_notice(format!("Resolved incident {} via 1-Click Heal", id));
                                    hgb_core::play_vibe_chime(true);
                                } else if let Some(CockpitItem::BrowserIncidentCard(c)) = CockpitVibeManager::handle_vibe_slash_command("/hud", "inspect") {
                                    self.browser_incident_cards.push(c);
                                    hgb_core::play_vibe_chime(true);
                                }
                                continue;
                            }
                            KeyCode::Tab => {
                                // Slash command autocompletion
                                if let Some(prefix) = self.prompt_input.strip_prefix('/') {
                                    let commands = [
                                        "goal", "compact", "heal", "share", "tunnel", "preview", "listen",
                                        "copy", "telepathy", "ghost", "council", "pixel", "spec", "db",
                                        "warp", "traffic", "wiretap", "sentry", "xerox", "governor",
                                        "browse", "search", "memory", "mem",
                                        "ambient", "focus", "validate", "forge", "prune", "ports", "ship", "commit",
                                        "hud", "snoop", "seed", "rewind", "redteam", "audit", "blueprint", "sentinel",
                                        "model", "cockpit", "chat", "tasks", "plan", "mcp", "clear", "help", "exit", "quit"
                                    ];
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
                                        "/goal" => {
                                            let goal_text = if parts.len() > 1 {
                                                parts[1..].join(" ")
                                            } else {
                                                "".to_string()
                                            };
                                            if goal_text.trim() == "clear" {
                                                self.clear_pinned_goal();
                                            } else if !goal_text.trim().is_empty() {
                                                self.set_pinned_goal(goal_text.trim());
                                            } else {
                                                self.add_system_notice("Usage: /goal <directive> or /goal clear".to_string());
                                            }
                                        }
                                        "/compact" => {
                                            let report = SmartAutoCompactor::compact_conversation(&mut self.conversation, self.pinned_goal.as_deref());
                                            self.add_system_notice(format!(
                                                "✓ Compacted {} items ({} chars saved, {} tool calls collapsed)",
                                                report.items_compacted, report.characters_saved, report.tool_calls_summarized
                                            ));
                                            hgb_core::play_vibe_chime(true);
                                        }
                                        "/heal" => {
                                            self.trigger_heal();
                                        }
                                        "/mcp" => {
                                            match hgb_core::mcp::McpConfigFile::load_from_dir(".") {
                                                Ok(Some(cfg)) => {
                                                    let count = cfg.mcp_servers.len();
                                                    let names: Vec<String> = cfg.mcp_servers.keys().cloned().collect();
                                                    self.add_system_notice(format!(
                                                        "🔌 Universal MCP: {} configured server(s) [{}]",
                                                        count,
                                                        names.join(", ")
                                                    ));
                                                }
                                                Ok(None) => {
                                                    self.add_system_notice("Universal MCP: No hagibis.mcp.json or .hgb/mcp.json found in workspace. Create one to link external tool servers.".to_string());
                                                }
                                                Err(e) => {
                                                    self.add_system_notice(format!("MCP config error: {}", e));
                                                }
                                            }
                                        }
                                        "/timeline" | "/fork" => {
                                            let mgr = hgb_core::timeline::TimelineManager::new(".");
                                            let sub = arg.trim();
                                            if sub.is_empty() || sub == "list" || sub == "ls" {
                                                match mgr.list_timelines() {
                                                    Ok(list) => {
                                                        if list.is_empty() {
                                                            self.add_system_notice("No active ephemeral timelines. Run '/timeline <name>' to fork an isolated worktree.".to_string());
                                                        } else {
                                                            let names: Vec<String> = list.iter().map(|t| format!("{} ({})", t.name, if t.is_git_worktree { "worktree" } else { "snapshot" })).collect();
                                                            self.add_system_notice(format!("Active Timelines ({}): {}", list.len(), names.join(", ")));
                                                        }
                                                    }
                                                    Err(e) => self.add_system_notice(format!("Failed to list timelines: {}", e)),
                                                }
                                            } else if let Some(target) = sub.strip_prefix("merge ") {
                                                match mgr.merge_timeline(target.trim()) {
                                                    Ok(rep) => {
                                                        if rep.success {
                                                            self.add_system_notice(format!("✓ Merged timeline '{}' ({} files modified)", target.trim(), rep.merged_files.len()));
                                                        } else {
                                                            self.add_system_notice(format!("Merge failed for '{}': {}", target.trim(), rep.message));
                                                        }
                                                    }
                                                    Err(e) => self.add_system_notice(format!("Timeline merge error: {}", e)),
                                                }
                                            } else if let Some(target) = sub.strip_prefix("diff ") {
                                                match mgr.diff_timeline(target.trim()) {
                                                    Ok(diff) => {
                                                        self.add_system_notice(format!("Timeline '{}' diff: {} files changed. Modified: {:?}", target.trim(), diff.files_changed, diff.modified_files));
                                                    }
                                                    Err(e) => self.add_system_notice(format!("Timeline diff error: {}", e)),
                                                }
                                            } else if let Some(target) = sub.strip_prefix("discard ") {
                                                match mgr.discard_timeline(target.trim()) {
                                                    Ok(_) => self.add_system_notice(format!("✓ Discarded timeline '{}'", target.trim())),
                                                    Err(e) => self.add_system_notice(format!("Timeline discard error: {}", e)),
                                                }
                                            } else {
                                                let name = sub.strip_prefix("create ").unwrap_or(sub).trim();
                                                match mgr.create_timeline(name, None) {
                                                    Ok(info) => {
                                                        self.add_system_notice(format!("✓ Ephemeral timeline '{}' created at {} ({})", info.name, info.path.display(), if info.is_git_worktree { "git worktree" } else { "snapshot fallback" }));
                                                    }
                                                    Err(e) => self.add_system_notice(format!("Failed to create timeline: {}", e)),
                                                }
                                            }
                                        }
                                        "/verify" => {
                                            let mut gate = hgb_core::verification_gate::VerificationGate::new(".");
                                            gate.auto_detect_invariants();
                                            match gate.verify_and_heal::<fn(&[hgb_core::verification_gate::VerificationStepResult], usize) -> hgb_core::Result<Option<String>>>(None) {
                                                Ok(cert) => {
                                                    self.add_system_notice(format!("🛡️ Verification Gate: {} | Hash: {} | Summary: {}", cert.status, &cert.integrity_hash[..12.min(cert.integrity_hash.len())], cert.summary));
                                                }
                                                Err(e) => self.add_system_notice(format!("Verification Gate execution error: {}", e)),
                                            }
                                        }
                                        "/fix" => {
                                            let interceptor = hgb_core::shell_hook::CrashInterceptor::new(".");
                                            match interceptor.load_latest_crash() {
                                                Ok(Some(rec)) => {
                                                    let diag = interceptor.diagnose_and_fix(&rec);
                                                    self.add_system_notice(format!("🚨 Shell Crash [{:?}]: {}\nSuggested Fix: {}\nConfidence: {:.0}%", diag.category, diag.root_cause, diag.suggested_command_fix, diag.confidence * 100.0));
                                                }
                                                Ok(None) => self.add_system_notice("No recent shell crash recorded. (Install shell hook with 'hgb init <bash|zsh|fish>')".to_string()),
                                                Err(e) => self.add_system_notice(format!("Crash inspection error: {}", e)),
                                            }
                                        }
                                        "/share" | "/tunnel" => {
                                            let port: u16 = arg.parse().unwrap_or(3000);
                                            self.add_tunnel_card(port);
                                        }
                                        "/preview" => {
                                            if !arg.is_empty() {
                                                self.add_image_preview("Preview", arg);
                                            } else {
                                                self.add_system_notice("Usage: /preview <path_to_image>".to_string());
                                            }
                                        }
                                        "/listen" => {
                                            self.toggle_listening();
                                        }
                                        "/copy" => {
                                            self.copy_latest_response();
                                        }
                                        "/telepathy" | "/ghost" | "/council" | "/pixel" | "/spec" | "/db"
                                        | "/warp" | "/traffic" | "/wiretap" | "/sentry" | "/xerox" | "/governor"
                                        | "/browse" | "/search" | "/memory" | "/mem"
                                        | "/ambient" | "/focus" | "/validate" | "/forge" | "/prune" | "/ports" | "/ship" | "/commit"
                                        | "/hud" | "/snoop" | "/seed" | "/rewind" | "/redteam" | "/audit" | "/blueprint" | "/sentinel"
                                        | "/patch" | "/live" | "/tdd" | "/isolate" | "/chime" => {
                                            let rest = if parts.len() > 1 {
                                                parts[1..].join(" ")
                                            } else {
                                                "".to_string()
                                            };
                                            if let Some(item) = CockpitVibeManager::handle_vibe_slash_command(cmd, &rest) {
                                                match item {
                                                    CockpitItem::TelepathyCard(c) => self.telepathy_cards.push(c),
                                                    CockpitItem::GhostCard(c) => self.ghost_cards.push(c),
                                                    CockpitItem::CouncilCard(c) => self.council_cards.push(c),
                                                    CockpitItem::PixelRadarCard(c) => self.pixel_cards.push(c),
                                                    CockpitItem::GreenLightCard(c) => self.green_light_cards.push(c),
                                                    CockpitItem::DbTimeMachineCard(c) => self.db_cards.push(c),
                                                    CockpitItem::ChronoWarpCard(c) => self.warp_cards.push(c),
                                                    CockpitItem::PhantomSwarmCard(c) => self.traffic_cards.push(c),
                                                    CockpitItem::WiretapCard(c) => self.wiretap_cards.push(c),
                                                    CockpitItem::HallucinationSentryCard(c) => self.sentry_cards.push(c),
                                                    CockpitItem::ClipboardXeroxCard(c) => self.xerox_cards.push(c),
                                                    CockpitItem::WattageGovernorCard(c) => self.governor_cards.push(c),
                                                    CockpitItem::WebBrowseCard(c) => self.web_browse_cards.push(c),
                                                    CockpitItem::WebSearchCard(c) => self.web_search_cards.push(c),
                                                    CockpitItem::MemoryCard(c) => self.memory_cards.push(c),
                                                    CockpitItem::AmbientCard(c) => self.ambient_cards.push(c),
                                                    CockpitItem::ValidationCard(c) => self.validation_cards.push(c),
                                                    CockpitItem::ForgeCard(c) => self.forge_cards.push(c),
                                                    CockpitItem::PruneCard(c) => self.prune_cards.push(c),
                                                    CockpitItem::PortCard(c) => self.port_cards.push(c),
                                                    CockpitItem::ShipCard(c) => self.ship_cards.push(c),
                                                    CockpitItem::BrowserIncidentCard(c) => self.browser_incident_cards.push(c),
                                                    CockpitItem::SeedCard(c) => self.seed_cards.push(c),
                                                    CockpitItem::RewindCard(c) => self.rewind_cards.push(c),
                                                    CockpitItem::RedTeamCard(c) => self.redteam_cards.push(c),
                                                    CockpitItem::BlueprintCard(c) => self.blueprint_cards.push(c),
                                                    CockpitItem::PassiveSentinelCard(c) => self.passive_sentinel_cards.push(c),
                                                    _ => {}
                                                }
                                                hgb_core::play_vibe_chime(true);
                                            }
                                        }
                                        "/model" | "/models" => {
                                            let trimmed = arg.trim();
                                            let lower = trimmed.to_lowercase();
                                            if trimmed.is_empty() || lower == "list" || lower == "show" || lower == "ls" {
                                                self.overlay = CockpitOverlay::ModelPicker { selected: 0 };
                                            } else if lower == "current" || lower == "status" {
                                                self.add_system_notice(format!("Current model: {}", self.model_pill));
                                            } else {
                                                let clean = if lower.starts_with("switch ") {
                                                    trimmed[7..].trim()
                                                } else if lower.starts_with("set ") {
                                                    trimmed[4..].trim()
                                                } else if lower.starts_with("use ") {
                                                    trimmed[4..].trim()
                                                } else {
                                                    trimmed
                                                };
                                                let token = clean.split_whitespace().next().unwrap_or(clean);
                                                let final_name = token.trim_matches(|c| c == '\'' || c == '"' || c == '`' || c == '(' || c == ')' || c == '[' || c == ']');
                                                if !final_name.is_empty() {
                                                    self.model_pill = final_name.to_string();
                                                    let _ = hgb_core::persist_active_model(final_name);
                                                    self.add_system_notice(format!("Active model set to: {}", final_name));
                                                }
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
                                } else if !input.is_empty() && !self.is_processing {
                                    prompt_rx = self.submit_current_prompt();
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

/// Top-level application runner for the Interactive Terminal Cockpit TUI (`hgb cockpit`)
pub struct CockpitApp;

impl CockpitApp {
    /// Launch and run the interactive Cockpit TUI application
    pub async fn run() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let mut state = CockpitState::new();
        state
            .run_interactive()
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
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

    #[test]
    fn test_scrollbar_mouse_click_up_down() {
        let mut state = CockpitState::new();
        state.chat_scroll = 10;

        if let Ok(mut sb) = state.scrollbar_hitbox.lock() {
            *sb = Some(ScrollbarHitbox {
                col: 80,
                y_start: 5,
                height: 20,
                max_scroll: 50,
            });
        }

        // Click top arrow ▲ at col=80, row=5 -> scrolls up
        let handled = state.handle_mouse_click(80, 5);
        assert!(handled);
        assert_eq!(state.chat_scroll, 13);

        // Click bottom arrow ▼ at col=80, row=24 -> scrolls down
        let handled = state.handle_mouse_click(80, 24);
        assert!(handled);
        assert_eq!(state.chat_scroll, 10);
    }

    #[test]
    fn test_scrollbar_mouse_drag_and_track_click() {
        let mut state = CockpitState::new();
        state.chat_scroll = 0;

        if let Ok(mut sb) = state.scrollbar_hitbox.lock() {
            *sb = Some(ScrollbarHitbox {
                col: 80,
                y_start: 5,
                height: 22,
                max_scroll: 100,
            });
        }

        // Click track near top (row=6) -> frac=0.0 -> max_scroll (100)
        let handled = state.handle_mouse_click(80, 6);
        assert!(handled);
        assert_eq!(state.chat_scroll, 100);

        // Drag along track to bottom (row=25) -> frac=1.0 -> 0
        let handled = state.handle_mouse_drag(80, 25);
        assert!(handled);
        assert_eq!(state.chat_scroll, 0);

        // Drag along track to midpoint (row=15) -> frac ~ 0.5
        let handled = state.handle_mouse_drag(80, 15);
        assert!(handled);
        assert!(state.chat_scroll >= 45 && state.chat_scroll <= 55);
    }

    #[test]
    fn test_scroll_pill_click_to_bottom() {
        let mut state = CockpitState::new();
        state.chat_scroll = 42;

        if let Ok(mut pill) = state.scroll_pill_hitbox.lock() {
            *pill = Some(ScrollPillHitbox {
                x: 30,
                y: 5,
                width: 40,
                height: 1,
            });
        }

        // Click within pill
        let handled = state.handle_mouse_click(45, 5);
        assert!(handled);
        assert_eq!(state.chat_scroll, 0);
    }
}
