use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use serde::{Deserialize, Serialize};
use crate::crud::{AgyCrud, CommandOptions, ReplaceOptions, ViewFileOptions};
use crate::error::{HgbError, Result};
use crate::security::AgentShieldLight;
use crate::traits::HgbProvider;

/// Roles within the ReAct agent conversation
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentRole {
    System,
    User,
    Assistant,
    Tool,
}

/// A parsed tool invocation requested by the model
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentToolCall {
    pub call_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

/// The result of executing a tool call
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentToolResult {
    pub call_id: String,
    pub tool_name: String,
    pub output: String,
    pub is_error: bool,
    pub duration_ms: u64,
}

/// A single turn in the ReAct conversation history
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub role: AgentRole,
    pub content: String,
    #[serde(default)]
    pub tool_calls: Vec<AgentToolCall>,
    #[serde(default)]
    pub tool_results: Vec<AgentToolResult>,
}

/// Configuration parameters for running the ReAct agent loop
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentLoopConfig {
    /// Maximum allowed reasoning turns (defaults to 25)
    pub max_turns: usize,
    /// Workspace root directory path
    pub workspace_root: PathBuf,
    /// Active model name override (optional)
    pub model: Option<String>,
    /// Auto-approve tool executions without interactive prompt
    pub auto_approve: bool,
    /// Timeout per turn in seconds (defaults to 120s)
    pub turn_timeout_secs: u64,
}

impl Default for AgentLoopConfig {
    fn default() -> Self {
        Self {
            max_turns: 25,
            workspace_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            model: None,
            auto_approve: true,
            turn_timeout_secs: 120,
        }
    }
}

/// Real-time streaming events dispatched by the ReAct engine
#[derive(Debug, Clone)]
pub enum AgentEvent {
    Thought(String),
    ToolStarting { call_id: String, tool_name: String, summary: String },
    ToolCompleted { call_id: String, tool_name: String, duration_ms: u64, is_error: bool, output_snippet: String },
    FinalResponse(String),
    Error(String),
}

/// Step summary for reports
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentStepInfo {
    pub step_index: usize,
    pub tool_name: String,
    pub tool_args: String,
    pub tool_result: String,
    pub success: bool,
    pub duration_ms: u64,
}

/// Session completion report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSessionReport {
    pub final_output: String,
    pub steps: Vec<AgentStepInfo>,
    pub total_duration_ms: u64,
    pub turns_taken: usize,
}

/// Autonomous ReAct Agent Execution Engine
pub struct ReActAgentEngine {
    provider: Arc<dyn HgbProvider>,
    config: AgentLoopConfig,
    history: Vec<AgentMessage>,
    system_prompt: Option<String>,
}

impl ReActAgentEngine {
    pub fn new(provider: Arc<dyn HgbProvider>, config: AgentLoopConfig) -> Self {
        Self {
            provider,
            config,
            history: Vec::new(),
            system_prompt: None,
        }
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Construct the complete system prompt including tool definitions and constraints
    pub fn build_system_prompt(
        &self,
        workspace_rules: Option<&str>,
        repomap: Option<&str>,
        memory_anchor: Option<&str>,
        style_guidance: Option<&str>,
    ) -> String {
        let mut prompt = String::from(
            "You are Hagibis (hgb), an elite Systems Rust & Autonomous AI Agent runtime.\n\
            You have full live internet browsing and persistent cross-session memory via your built-in tools.\n\
            Never state that you cannot access the internet, browse the web, or remember past decisions.\n\
            Always use `search_web` and `browse_web` when fresh information, external data, or documentation is needed, and use `record_memory` / `search_memory` to retain architectural context.\n\
            Always reason step-by-step.\n\n\
            ## Available Tools:\n\
            1. view_file(path: string, start_line?: number, end_line?: number, content_offset?: number)\n\
            2. write_to_file(path: string, content: string, overwrite: boolean, artifact_summary?: string)\n\
            3. replace_file_content(path: string, target_content: string, replacement_content: string, start_line?: number, end_line?: number, allow_multiple?: boolean)\n\
            4. run_command(command: string, cwd?: string, timeout_ms?: number)\n\
            5. grep_search(root: string, query: string, is_regex?: boolean, case_insensitive?: boolean, match_per_line?: boolean)\n\
            6. find_by_name(root: string, pattern?: string, extensions?: string[], excludes?: string[], max_depth?: number)\n\
            7. list_dir(path: string)\n\
            8. browse_web(url: string, max_tokens?: number)\n\
            9. search_web(query: string, max_results?: number)\n\
            10. record_memory(title: string, decision: string, context?: string, is_technical_debt?: boolean, severity?: string)\n\
            11. search_memory(query: string, max_results?: number)\n\
            12. record_style_feedback(snippet: string, accepted: boolean)\n\n\
            ## Tool Call Format:\n\
            When invoking a tool, emit a JSON block formatted exactly as:\n\
            ```tool_call\n\
            {\n\
              \"call_id\": \"call_1\",\n\
              \"tool_name\": \"<name>\",\n\
              \"arguments\": { ... }\n\
            }\n\
            ```\n\
            When you have completed the task and no further tool calls are required, provide your final response directly.\n"
        );

        if let Some(rules) = workspace_rules {
            prompt.push_str("\n## Workspace Rules & Instructions:\n");
            prompt.push_str(rules);
            prompt.push('\n');
        }

        if let Some(map) = repomap {
            prompt.push_str("\n## Repository AST Map:\n");
            prompt.push_str(map);
            prompt.push('\n');
        }

        let resolved_memory = if let Some(mem) = memory_anchor {
            if !mem.trim().is_empty() {
                Some(mem.to_string())
            } else {
                None
            }
        } else {
            crate::memory::ProjectMemoryLedger::load_or_init(&self.config.workspace_root)
                .ok()
                .map(|ledger| ledger.render_llm_anchor(1500))
                .filter(|s| !s.trim().is_empty())
        };

        if let Some(mem_str) = resolved_memory {
            prompt.push_str("\n## Living Project Memory & Architectural Ledger:\n");
            prompt.push_str(&mem_str);
            prompt.push('\n');
        }

        let resolved_style = if let Some(style) = style_guidance {
            if !style.trim().is_empty() {
                Some(style.to_string())
            } else {
                None
            }
        } else {
            let vault_path = self.config.workspace_root.join(".hgb").join("style_vault.db");
            if vault_path.exists() {
                crate::style::StyleMemoryVault::new(&vault_path)
                    .ok()
                    .map(|v| v.render_prompt_guidance(5))
                    .filter(|s| !s.trim().is_empty())
            } else {
                None
            }
        };

        if let Some(style_str) = resolved_style {
            prompt.push_str("\n## Learned Style Guidance:\n");
            prompt.push_str(&style_str);
            prompt.push('\n');
        }

        prompt
    }

    /// Execute the full multi-turn autonomous loop
    pub async fn run<F>(&mut self, user_goal: &str, mut event_sink: F) -> Result<AgentSessionReport>
    where
        F: FnMut(AgentEvent) + Send + 'static,
    {
        let start_time = Instant::now();
        let mut steps = Vec::new();

        // 1. Initialize system message (using configured prompt or auto-discovering memory & style)
        let sys_content = self.system_prompt.clone().unwrap_or_else(|| {
            self.build_system_prompt(None, None, None, None)
        });
        self.history.push(AgentMessage {
            role: AgentRole::System,
            content: sys_content,
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
        });

        // 2. Initialize conversation with user goal
        self.history.push(AgentMessage {
            role: AgentRole::User,
            content: user_goal.to_string(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
        });

        let mut turn = 0;
        while turn < self.config.max_turns {
            turn += 1;

            // 3. Synthesize prompt history for completion
            let aggregated_prompt = self.format_conversation_prompt();
            let model_arg = self.config.model.as_deref();

            let response = self.provider.complete(&aggregated_prompt, model_arg).await?;

            // 4. Parse tool calls
            let tool_calls = Self::extract_tool_calls(&response);

            if tool_calls.is_empty() {
                // Final answer reached
                event_sink(AgentEvent::FinalResponse(response.clone()));
                self.history.push(AgentMessage {
                    role: AgentRole::Assistant,
                    content: response.clone(),
                    tool_calls: Vec::new(),
                    tool_results: Vec::new(),
                });

                let total_duration_ms = start_time.elapsed().as_millis() as u64;
                return Ok(AgentSessionReport {
                    final_output: response,
                    steps,
                    total_duration_ms,
                    turns_taken: turn,
                });
            }

            // 5. Notify thoughts before tool calls
            let thought_content = Self::strip_tool_calls(&response);
            if !thought_content.trim().is_empty() {
                event_sink(AgentEvent::Thought(thought_content.clone()));
            }

            // 6. Execute each tool call
            let mut results = Vec::new();
            for call in &tool_calls {
                let summary = format!("{}({})", call.tool_name, serde_json::to_string(&call.arguments).unwrap_or_default());
                event_sink(AgentEvent::ToolStarting {
                    call_id: call.call_id.clone(),
                    tool_name: call.tool_name.clone(),
                    summary: summary.clone(),
                });

                let tool_start = Instant::now();
                let tool_res = self.execute_tool(&call.tool_name, &call.arguments).await;
                let duration_ms = tool_start.elapsed().as_millis() as u64;

                let (output, is_error) = match tool_res {
                    Ok(out) => (out, false),
                    Err(e) => (format!("Tool execution error: {}", e), true),
                };

                let snippet = if output.len() > 300 {
                    format!("{}...", &output[..300])
                } else {
                    output.clone()
                };

                event_sink(AgentEvent::ToolCompleted {
                    call_id: call.call_id.clone(),
                    tool_name: call.tool_name.clone(),
                    duration_ms,
                    is_error,
                    output_snippet: snippet,
                });

                steps.push(AgentStepInfo {
                    step_index: steps.len() + 1,
                    tool_name: call.tool_name.clone(),
                    tool_args: summary,
                    tool_result: output.clone(),
                    success: !is_error,
                    duration_ms,
                });

                results.push(AgentToolResult {
                    call_id: call.call_id.clone(),
                    tool_name: call.tool_name.clone(),
                    output,
                    is_error,
                    duration_ms,
                });
            }

            // 7. Record turn to history
            self.history.push(AgentMessage {
                role: AgentRole::Assistant,
                content: response,
                tool_calls,
                tool_results: results,
            });
        }

        Err(HgbError::Execution(format!(
            "ReAct agent reached maximum turn limit ({}) without finalizing",
            self.config.max_turns
        )))
    }

    /// Dispatch tool calls to AgyCrud
    pub async fn execute_tool(&self, name: &str, args: &serde_json::Value) -> Result<String> {
        AgentShieldLight::scan_tool_call(name, args)?;

        match name {
            "run_command" | "exec" | "sh" | "bash" => {
                let command = args.get("command")
                    .or_else(|| args.get("CommandLine"))
                    .or_else(|| args.get("cmd"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let cwd = args.get("cwd")
                    .or_else(|| args.get("Cwd"))
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| self.config.workspace_root.clone());
                let timeout_ms = args.get("timeout_ms")
                    .or_else(|| args.get("WaitMsBeforeAsync"))
                    .and_then(|v| v.as_u64());

                let options = CommandOptions {
                    cwd: Some(cwd.clone()),
                    timeout_ms,
                    env: HashMap::new(),
                    max_output_bytes: Some(256 * 1024),
                    wait_ms_before_async: None,
                };
                let res = AgyCrud::run_command(command, Some(&cwd), options).await?;
                Ok(format!(
                    "Exit Code: {}\nCombined Output:\n{}",
                    res.exit_code, res.combined_output
                ))
            }
            "view_file" | "cat" | "read" => {
                let path = args.get("path")
                    .or_else(|| args.get("AbsolutePath"))
                    .or_else(|| args.get("TargetFile"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let options = ViewFileOptions::from_json(args);
                let res = AgyCrud::view_file(path, options)?;
                Ok(res.content)
            }
            "write_to_file" | "write" => {
                let path = args.get("path")
                    .or_else(|| args.get("TargetFile"))
                    .or_else(|| args.get("AbsolutePath"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let content = args.get("content")
                    .or_else(|| args.get("CodeContent"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let overwrite = args.get("overwrite")
                    .or_else(|| args.get("Overwrite"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let summary = args.get("artifact_summary")
                    .or_else(|| args.get("Description"))
                    .and_then(|v| v.as_str());
                AgyCrud::write_to_file(path, content, overwrite, summary)
            }
            "replace_file_content" | "edit" | "replace" => {
                let path = args.get("path")
                    .or_else(|| args.get("TargetFile"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let target = args.get("target_content")
                    .or_else(|| args.get("TargetContent"))
                    .or_else(|| args.get("target"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let replacement = args.get("replacement_content")
                    .or_else(|| args.get("ReplacementContent"))
                    .or_else(|| args.get("replacement"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let options = ReplaceOptions::from_json(args);
                AgyCrud::replace_file_content(path, target, replacement, options)
            }
            "grep_search" | "grep" => {
                let root = args.get("root")
                    .or_else(|| args.get("SearchPath"))
                    .or_else(|| args.get("path"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(".");
                let query = args.get("query")
                    .or_else(|| args.get("Query"))
                    .or_else(|| args.get("pattern"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let is_regex = args.get("is_regex")
                    .or_else(|| args.get("IsRegex"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let case_insensitive = args.get("case_insensitive")
                    .or_else(|| args.get("CaseInsensitive"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let match_per_line = args.get("match_per_line")
                    .or_else(|| args.get("MatchPerLine"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let res = AgyCrud::grep_search(root, query, is_regex, case_insensitive, match_per_line, &[])?;
                Ok(serde_json::to_string_pretty(&res).unwrap_or_default())
            }
            "find_by_name" | "find" | "fd" => {
                let root = args.get("root")
                    .or_else(|| args.get("SearchDirectory"))
                    .or_else(|| args.get("dir"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(".");
                let pattern = args.get("pattern")
                    .or_else(|| args.get("Pattern"))
                    .and_then(|v| v.as_str());
                let entries = AgyCrud::find_by_name(root, pattern, &[], &[], None, None)?;
                Ok(serde_json::to_string_pretty(&entries).unwrap_or_default())
            }
            "list_dir" | "ls" | "dir" => {
                let path = args.get("path")
                    .or_else(|| args.get("DirectoryPath"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(".");
                let entries = AgyCrud::list_dir(path)?;
                Ok(serde_json::to_string_pretty(&entries).unwrap_or_default())
            }
            "browse_web" | "browse" | "fetch_url" | "web_browse" => {
                let url = args.get("url")
                    .or_else(|| args.get("Url"))
                    .or_else(|| args.get("link"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let max_tokens = args.get("max_tokens")
                    .or_else(|| args.get("MaxTokens"))
                    .or_else(|| args.get("budget"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1500) as usize;

                let engine = crate::web_browser::WebBrowserEngine::default();
                let res = engine.browse_url(url, max_tokens).await?;
                Ok(format!(
                    "# {}\nSource: {}\nStatus: {}\nTokens: {}{}\n\n{}",
                    res.title,
                    res.url,
                    res.status_code,
                    res.token_count,
                    if res.truncated { " (truncated)" } else { "" },
                    res.markdown
                ))
            }
            "search_web" | "search" | "web_search" => {
                let query = args.get("query")
                    .or_else(|| args.get("Query"))
                    .or_else(|| args.get("q"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let max_results = args.get("max_results")
                    .or_else(|| args.get("MaxResults"))
                    .or_else(|| args.get("limit"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(5) as usize;

                let engine = crate::web_browser::WebBrowserEngine::default();
                let results = engine.search_web(query, max_results).await?;
                Ok(serde_json::to_string_pretty(&results).unwrap_or_default())
            }
            "record_memory" | "remember" => {
                let title = args.get("title")
                    .or_else(|| args.get("Title"))
                    .or_else(|| args.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("Untitled Decision");
                let decision = args.get("decision")
                    .or_else(|| args.get("Decision"))
                    .or_else(|| args.get("description"))
                    .or_else(|| args.get("content"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let context = args.get("context")
                    .or_else(|| args.get("Context"))
                    .or_else(|| args.get("rationale"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let is_debt = args.get("is_technical_debt")
                    .or_else(|| args.get("is_tech_debt"))
                    .or_else(|| args.get("debt"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let severity_str = args.get("severity")
                    .or_else(|| args.get("Severity"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("Medium");

                let mut ledger = crate::memory::ProjectMemoryLedger::load_or_init(&self.config.workspace_root)?;
                if is_debt {
                    let severity = match severity_str.to_lowercase().as_str() {
                        "low" => crate::memory::DebtSeverity::Low,
                        "high" => crate::memory::DebtSeverity::High,
                        "critical" => crate::memory::DebtSeverity::Critical,
                        _ => crate::memory::DebtSeverity::Medium,
                    };
                    let files: Vec<String> = args.get("affected_files")
                        .or_else(|| args.get("files"))
                        .and_then(|v| v.as_array())
                        .map(|arr| arr.iter().filter_map(|s| s.as_str().map(|x| x.to_string())).collect())
                        .unwrap_or_default();
                    let debt_id = ledger.record_tech_debt(title, decision, severity, files)?;
                    Ok(format!(
                        "Successfully recorded Technical Debt [{}]: {}\nSeverity: {}\nStored in: {}",
                        debt_id, title, severity, ledger.file_path.display()
                    ))
                } else {
                    let adr_id = ledger.record_decision(title, decision, context)?;
                    Ok(format!(
                        "Successfully recorded Architectural Decision Record [{}]: {}\nDecision: {}\nStored in: {}",
                        adr_id, title, decision, ledger.file_path.display()
                    ))
                }
            }
            "search_memory" | "find_memory" => {
                let query = args.get("query")
                    .or_else(|| args.get("Query"))
                    .or_else(|| args.get("q"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let max_results = args.get("max_results")
                    .or_else(|| args.get("MaxResults"))
                    .or_else(|| args.get("limit"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10) as usize;

                let ledger = crate::memory::ProjectMemoryLedger::load_or_init(&self.config.workspace_root)?;
                let matched_decisions = ledger.search_decisions(query);
                let matched_debts = ledger.search_debts(query);

                let vault_path = self.config.workspace_root.join(".hgb").join("style_vault.db");
                let matched_styles = if vault_path.exists() {
                    crate::style::StyleMemoryVault::new(&vault_path)
                        .ok()
                        .and_then(|v| v.search_feedback(query, max_results).ok())
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };

                if matched_decisions.is_empty() && matched_debts.is_empty() && matched_styles.is_empty() {
                    return Ok(format!("No memory entries found matching '{}'", query));
                }

                let mut out = format!("# Memory Search Results for \"{}\":\n\n", query);

                if !matched_decisions.is_empty() {
                    out.push_str(&format!("## Architectural Decisions ({}):\n", matched_decisions.len()));
                    for d in matched_decisions.iter().take(max_results) {
                        out.push_str(&format!(
                            "- [{}] ({}): {} => {}\n  Context: {}\n",
                            d.id, d.status, d.title, d.decision, d.context
                        ));
                    }
                    out.push('\n');
                }

                if !matched_debts.is_empty() {
                    out.push_str(&format!("## Technical Debt ({}):\n", matched_debts.len()));
                    for debt in matched_debts.iter().take(max_results) {
                        let files_str = if debt.affected_files.is_empty() {
                            String::new()
                        } else {
                            format!(" (in [{}])", debt.affected_files.join(", "))
                        };
                        out.push_str(&format!(
                            "- [{}] (Severity: {}): {}{}\n  Description: {}\n",
                            debt.id, debt.severity, debt.title, files_str, debt.description
                        ));
                    }
                    out.push('\n');
                }

                if !matched_styles.is_empty() {
                    out.push_str(&format!("## Style Feedback ({}):\n", matched_styles.len()));
                    for sf in matched_styles.iter().take(max_results) {
                        let tag = if sf.accepted { "ACCEPTED PATTERN" } else { "REJECTED ANTI-PATTERN" };
                        out.push_str(&format!("- [{}] {}\n", tag, sf.snippet));
                    }
                    out.push('\n');
                }

                Ok(out)
            }
            "record_style_feedback" | "style_feedback" => {
                let snippet = args.get("snippet")
                    .or_else(|| args.get("Snippet"))
                    .or_else(|| args.get("pattern"))
                    .or_else(|| args.get("code"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let accepted = args.get("accepted")
                    .or_else(|| args.get("Accepted"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);

                if snippet.trim().is_empty() {
                    return Err(HgbError::Execution("Style feedback snippet cannot be empty".to_string()));
                }

                let vault_path = self.config.workspace_root.join(".hgb").join("style_vault.db");
                let vault = crate::style::StyleMemoryVault::new(&vault_path)?;
                vault.record_feedback(snippet, accepted)?;

                let category = if accepted { "Accepted Convention" } else { "Rejected Anti-Pattern" };
                Ok(format!(
                    "Successfully recorded style feedback [{}]:\n'{}'\nPersisted to: {}",
                    category, snippet, vault_path.display()
                ))
            }
            _ => Err(HgbError::Execution(format!("Unknown tool '{}'", name))),
        }
    }

    /// Extract tool calls from assistant message
    pub fn extract_tool_calls(text: &str) -> Vec<AgentToolCall> {
        let mut calls = Vec::new();
        let marker = "```tool_call";
        let mut search_from = 0;

        while let Some(start_idx) = text[search_from..].find(marker) {
            let actual_start = search_from + start_idx + marker.len();
            if let Some(end_idx) = text[actual_start..].find("```") {
                let json_slice = text[actual_start..actual_start + end_idx].trim();
                if let Ok(call) = serde_json::from_str::<AgentToolCall>(json_slice) {
                    calls.push(call);
                }
                search_from = actual_start + end_idx + 3;
            } else {
                break;
            }
        }
        calls
    }

    /// Strip tool call code blocks from text to get thought process
    pub fn strip_tool_calls(text: &str) -> String {
        let mut out = String::new();
        let lines: Vec<&str> = text.lines().collect();
        let mut in_block = false;

        for line in lines {
            if line.trim().starts_with("```tool_call") {
                in_block = true;
                continue;
            }
            if in_block && line.trim().starts_with("```") {
                in_block = false;
                continue;
            }
            if !in_block {
                out.push_str(line);
                out.push('\n');
            }
        }
        out
    }

    fn format_conversation_prompt(&self) -> String {
        let mut out = String::new();
        for msg in &self.history {
            match msg.role {
                AgentRole::System => {
                    out.push_str(&format!("<system>\n{}\n</system>\n\n", msg.content));
                }
                AgentRole::User => {
                    out.push_str(&format!("<user>\n{}\n</user>\n\n", msg.content));
                }
                AgentRole::Assistant => {
                    out.push_str(&format!("<assistant>\n{}\n</assistant>\n\n", msg.content));
                    for res in &msg.tool_results {
                        out.push_str(&format!(
                            "<observation call_id=\"{}\" tool=\"{}\" is_error=\"{}\">\n{}\n</observation>\n\n",
                            res.call_id, res.tool_name, res.is_error, res.output
                        ));
                    }
                }
                AgentRole::Tool => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct MockProvider {
        responses: tokio::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl HgbProvider for MockProvider {
        fn name(&self) -> &str {
            "mock"
        }

        async fn complete(&self, _prompt: &str, _model: Option<&str>) -> Result<String> {
            let mut guard = self.responses.lock().await;
            if guard.is_empty() {
                Ok("Final answer: everything done!".to_string())
            } else {
                Ok(guard.remove(0))
            }
        }
    }

    #[tokio::test]
    async fn test_extract_and_strip_tool_calls() {
        let text = "I will list the directory.\n```tool_call\n{\n  \"call_id\": \"1\",\n  \"tool_name\": \"list_dir\",\n  \"arguments\": {\"path\": \".\"}\n}\n```\nDone.";
        let calls = ReActAgentEngine::extract_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_name, "list_dir");

        let stripped = ReActAgentEngine::strip_tool_calls(text);
        assert!(stripped.contains("I will list the directory."));
        assert!(stripped.contains("Done."));
        assert!(!stripped.contains("```tool_call"));
    }

    #[tokio::test]
    async fn test_react_agent_execution_loop() {
        let mock_responses = vec![
            "Thinking... Let me check files.\n```tool_call\n{\n  \"call_id\": \"c1\",\n  \"tool_name\": \"run_command\",\n  \"arguments\": {\"command\": \"echo 'hello vibe coder'\"}\n}\n```".to_string(),
            "All done! The command output verified.".to_string(),
        ];

        let provider = Arc::new(MockProvider {
            responses: tokio::sync::Mutex::new(mock_responses),
        });

        let mut engine = ReActAgentEngine::new(provider, AgentLoopConfig::default());
        let report = engine.run("Test task", |_| {}).await.unwrap();

        assert_eq!(report.steps.len(), 1);
        assert_eq!(report.steps[0].tool_name, "run_command");
        assert!(report.steps[0].tool_result.contains("hello vibe coder"));
        assert!(report.final_output.contains("All done!"));
    }
}
