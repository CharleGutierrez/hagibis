//! # Interactive Cockpit TUI Engine for Hagibis (`hgb cockpit`)
//!
//! Provides live DAG visual tracking, mid-flight agent steering (Pause, EditScratchpad,
//! RedirectTool, Resume), and real-time telemetry rendering using Ratatui.

use chrono::Utc;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use hgb_core::{HgbError, Result};
use ratatui::{
    backend::{CrosstermBackend, TestBackend},
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Gauge, List, ListItem, Paragraph, Row, Table},
    Frame, Terminal,
};
use serde::{Deserialize, Serialize};
use std::io::{self, stdout};
use std::time::Duration;

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

/// A node in the DAG monitored and steered by the Cockpit
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CockpitDagNode {
    pub id: String,
    pub name: String,
    pub model: String,
    pub status: CockpitNodeStatus,
    pub tokens_used: u64,
    pub scratchpad: String,
    pub tool_calls: Vec<String>,
    pub dependencies: Vec<String>,
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
        }
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
}

impl Default for CockpitState {
    fn default() -> Self {
        Self::new()
    }
}

impl CockpitState {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            selected_index: 0,
            telemetry: CockpitTelemetry::default(),
            steering_history: Vec::new(),
            log_feed: vec!["[SYSTEM] Hagibis Interactive Cockpit initialized.".to_string()],
            is_paused: false,
        }
    }

    /// Add a node to the visual DAG tracker
    pub fn add_node(&mut self, node: CockpitDagNode) {
        self.nodes.push(node);
    }

    /// Update status of a DAG node
    pub fn update_node_status(&mut self, node_id: &str, status: CockpitNodeStatus) {
        if let Some(node) = self.nodes.iter_mut().find(|n| n.id == node_id) {
            node.status = status;
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
            SteeringAction::RedirectTool { node_id, new_tool_name, .. } => {
                let node = self.nodes.iter_mut().find(|n| &n.id == node_id).ok_or_else(|| {
                    HgbError::Execution(format!("Node '{}' not found", node_id))
                })?;
                node.tool_calls.push(format!("REDIRECTED -> {}", new_tool_name));
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

    /// Render Cockpit UI layout into Ratatui Frame
    pub fn render_ui(&self, frame: &mut Frame) {
        let area = frame.area();

        // 3-way vertical split: Top Telemetry Bar (3), Main Body (Min 10), Bottom Logs/Controls (7)
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(10),
                Constraint::Length(7),
            ])
            .split(area);

        // --- 1. Top Telemetry Bar ---
        let tel_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(25),
                Constraint::Percentage(25),
                Constraint::Percentage(25),
                Constraint::Percentage(25),
            ])
            .split(main_chunks[0]);

        // Peak Thermals
        let temp_color = if self.telemetry.peak_temperature_celsius > 75.0 { Color::Red } else { Color::Green };
        let temp_p = Paragraph::new(format!("{:.1}°C", self.telemetry.peak_temperature_celsius))
            .style(Style::default().fg(temp_color).add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL).title("🔥 Peak Thermal"));
        frame.render_widget(temp_p, tel_chunks[0]);

        // Throughput
        let speed_p = Paragraph::new(format!("{:.1} tok/s | Total: {}", self.telemetry.tokens_per_sec, self.telemetry.total_tokens))
            .style(Style::default().fg(Color::Cyan))
            .block(Block::default().borders(Borders::ALL).title("⚡ Throughput"));
        frame.render_widget(speed_p, tel_chunks[1]);

        // Lakandiwa Entropy
        let entropy_color = if self.telemetry.lakandiwa_entropy_bits > 1.75 { Color::Red } else { Color::Green };
        let entropy_p = Paragraph::new(format!("{:.2} bits", self.telemetry.lakandiwa_entropy_bits))
            .style(Style::default().fg(entropy_color))
            .block(Block::default().borders(Borders::ALL).title("🧠 Lakandiwa Entropy"));
        frame.render_widget(entropy_p, tel_chunks[2]);

        // Acceptance Gauge
        let accept_pct = (self.telemetry.speculative_acceptance_rate * 100.0) as u16;
        let gauge = Gauge::default()
            .block(Block::default().borders(Borders::ALL).title("🎯 Draft Acceptance"))
            .gauge_style(Style::default().fg(Color::Yellow))
            .percent(accept_pct.min(100));
        frame.render_widget(gauge, tel_chunks[3]);

        // --- 2. Main Middle Section: Split Left (DAG Nodes) & Right (Inspector) ---
        let middle_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(45),
                Constraint::Percentage(55),
            ])
            .split(main_chunks[1]);

        // Left Pane: DAG Node Table / List
        let rows: Vec<Row> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(idx, node)| {
                let (badge, color) = node.status.badge();
                let is_sel = idx == self.selected_index;
                let marker = if is_sel { "▶ " } else { "  " };

                let cells = vec![
                    Cell::from(format!("{}{}", marker, node.name)),
                    Cell::from(badge).style(Style::default().fg(color).add_modifier(Modifier::BOLD)),
                    Cell::from(node.model.as_str()),
                    Cell::from(format!("{}", node.tokens_used)),
                ];

                let row = Row::new(cells);
                if is_sel {
                    row.style(Style::default().bg(Color::Rgb(30, 30, 50)).add_modifier(Modifier::BOLD))
                } else {
                    row
                }
            })
            .collect();

        let table = Table::new(
            rows,
            [
                Constraint::Percentage(40),
                Constraint::Percentage(25),
                Constraint::Percentage(20),
                Constraint::Percentage(15),
            ],
        )
        .header(
            Row::new(vec!["Node", "Status", "Model", "Tokens"])
                .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" 🗺️ Swarm DAG Nodes ({}) ", self.nodes.len())),
        );
        frame.render_widget(table, middle_chunks[0]);

        // Right Pane: Selected Node Inspector
        let inspector_content = if let Some(node) = self.selected_node() {
            vec![
                Line::from(vec![
                    Span::styled("Node ID: ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                    Span::raw(&node.id),
                ]),
                Line::from(vec![
                    Span::styled("Active Model: ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                    Span::raw(&node.model),
                ]),
                Line::from(vec![
                    Span::styled("Tool Invocations: ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                    Span::raw(format!("{}", node.tool_calls.len())),
                ]),
                Line::from(""),
                Line::from(Span::styled("─── Agent Scratchpad / Memory ───", Style::default().fg(Color::Cyan))),
                Line::from(node.scratchpad.as_str()),
            ]
        } else {
            vec![Line::from("No DAG nodes configured.")]
        };

        let inspector_p = Paragraph::new(inspector_content)
            .block(Block::default().borders(Borders::ALL).title(" 🔬 Node Inspector & Scratchpad "))
            .alignment(Alignment::Left);
        frame.render_widget(inspector_p, middle_chunks[1]);

        // --- 3. Bottom Pane: Live Log Feed & Mid-flight Steering Controls ---
        let bottom_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(65),
                Constraint::Percentage(35),
            ])
            .split(main_chunks[2]);

        let log_items: Vec<ListItem> = self
            .log_feed
            .iter()
            .rev()
            .take(5)
            .map(|msg| ListItem::new(msg.as_str()))
            .collect();
        let log_list = List::new(log_items).block(Block::default().borders(Borders::ALL).title(" 📜 Audit Log Feed "));
        frame.render_widget(log_list, bottom_chunks[0]);

        let controls_text = vec![
            Line::from(vec![
                Span::styled("[P] ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::raw("Pause Node   "),
                Span::styled("[R] ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                Span::raw("Resume"),
            ]),
            Line::from(vec![
                Span::styled("[E] ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                Span::raw("Edit Scratch "),
                Span::styled("[T] ", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
                Span::raw("Redirect Tool"),
            ]),
            Line::from(vec![
                Span::styled("[A] ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                Span::raw("Abort Node   "),
                Span::styled("[Q] ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::raw("Quit Cockpit"),
            ]),
        ];
        let controls_p = Paragraph::new(controls_text)
            .block(Block::default().borders(Borders::ALL).title(" 🎮 Mid-Flight Steering "));
        frame.render_widget(controls_p, bottom_chunks[1]);
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

    /// Launch interactive terminal cockpit session
    pub fn run_interactive(&mut self) -> io::Result<()> {
        enable_raw_mode()?;
        let mut stdout_handle = stdout();
        crossterm::execute!(stdout_handle, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout_handle);
        let mut terminal = Terminal::new(backend)?;

        let res = self.event_loop(&mut terminal);

        disable_raw_mode()?;
        crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        terminal.show_cursor()?;

        res
    }

    fn event_loop<B: ratatui::backend::Backend>(&mut self, terminal: &mut Terminal<B>) -> io::Result<()> {
        loop {
            terminal.draw(|f| self.render_ui(f))?;

            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Release {
                        continue;
                    }
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => break,
                        KeyCode::Down | KeyCode::Char('j') => self.select_next(),
                        KeyCode::Up | KeyCode::Char('k') => self.select_prev(),
                        KeyCode::Char('p') | KeyCode::Char('P') => {
                            if let Some(node) = self.selected_node() {
                                let id = node.id.clone();
                                let _ = self.apply_steering(SteeringAction::Pause { node_id: id });
                            }
                        }
                        KeyCode::Char('r') | KeyCode::Char('R') => {
                            if let Some(node) = self.selected_node() {
                                let id = node.id.clone();
                                let _ = self.apply_steering(SteeringAction::Resume { node_id: id });
                            }
                        }
                        KeyCode::Char('e') | KeyCode::Char('E') => {
                            if let Some(node) = self.selected_node() {
                                let id = node.id.clone();
                                let _ = self.apply_steering(SteeringAction::EditScratchpad {
                                    node_id: id,
                                    new_scratchpad: "Steered via Interactive Cockpit keystroke".to_string(),
                                });
                            }
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
                        }
                        KeyCode::Char('a') | KeyCode::Char('A') => {
                            if let Some(node) = self.selected_node() {
                                let id = node.id.clone();
                                let _ = self.apply_steering(SteeringAction::Abort {
                                    node_id: id,
                                    reason: "Manual supervisor abort in Cockpit".to_string(),
                                });
                            }
                        }
                        _ => {}
                    }
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
    fn test_cockpit_headless_render() {
        let mut state = CockpitState::new();
        state.add_node(CockpitDagNode::new("n1", "Root Task", "gemini-2.5-pro"));
        let buffer = state.render_headless(100, 30);
        assert!(buffer.content.len() >= 100 * 30);
    }
}
