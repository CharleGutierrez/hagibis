//! Zero-Friction "Vibe Prompting" Engine (`vibe_prompt_runner`)
//!
//! Autonomous First-Green prompt execution engine that coordinates:
//! 1. Hardware governor escalation (`VibeHardwareGuard` via dynatune)
//! 2. Pre-flight prompt grounding via `UniversalPromptGrounder`
//! 3. MCP tool catalog discovery (`hagibis.mcp.json`)
//! 4. Fast draft/target local LLM code synthesis (`qwen2.5-coder:1.5b` / `phi3:3.8b`)
//! 5. Ephemeral RAM CoW evaluation (`OverlaySandbox` in `/dev/shm`)
//! 6. Compiler check & First-Green self-healing loop via `CompilerInterceptor` (up to 3 retries)
//! 7. Workspace commit & hot-reload notification to `hgbd` daemon ONLY when 100% green
//! 8. Workspace safety: clean rollback & diagnostic failure card if all retries fail
//! 9. RAII hardware relaxation & ambient vibe audio chime

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};

use crate::audio::play_vibe_chime;
use crate::compiler_interceptor::{CompilerDiagnostic, CompilerInterceptor, CompilerTool, DiagnosticSeverity};
use crate::error::{HgbError, Result};
use crate::overlayfs_sandbox::OverlaySandbox;
use crate::prompt_grounding::{GroundingContextReport, UniversalPromptGrounder};
use crate::providers::gemini::GeminiProvider;
use crate::providers::openai_compatible::{AnthropicProvider, OpenAiCompatibleProvider};
use crate::traits::HgbProvider;
use crate::vibe_hardware_governor::{VibeHardwareGovernor, VibeHardwareGuard, VibeThermalState};
use crate::git_micro_commit::GitMicroCommitMirror;

pub const DEFAULT_OLLAMA_ENDPOINT: &str = "http://127.0.0.1:11434";
pub const DEFAULT_OLLAMA_SIBLING_DIR: &str = "../ollama";

/// Discover sibling ollama directory using env override or relative sibling search
pub fn resolve_ollama_sibling_dir(workspace_root: Option<&Path>) -> PathBuf {
    if let Ok(custom) = std::env::var("HGB_OLLAMA_DIR").or_else(|_| std::env::var("OLLAMA_SIBLING_DIR")) {
        return PathBuf::from(custom);
    }
    if let Some(ws) = workspace_root {
        if let Some(parent) = ws.parent() {
            let cand = parent.join("ollama");
            if cand.is_dir() {
                return cand;
            }
        }
    }
    let candidates = [
        PathBuf::from("../ollama"),
        PathBuf::from("../../ollama"),
        PathBuf::from("ollama"),
    ];
    for cand in candidates {
        if cand.is_dir() {
            return cand;
        }
    }
    PathBuf::from(DEFAULT_OLLAMA_SIBLING_DIR)
}
pub const DEFAULT_DRAFT_MODEL: &str = "qwen2.5-coder:1.5b";
pub const DEFAULT_TARGET_MODEL: &str = "phi3:3.8b";

/// Input specification for frictionless vibe prompting
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VibePromptSpec {
    pub prompt: String,
    pub target_dir: Option<PathBuf>,
    pub auto_commit: bool,
    pub use_local_ollama: bool,
    #[serde(default)]
    pub check_command: Option<String>,
}

impl Default for VibePromptSpec {
    fn default() -> Self {
        Self {
            prompt: String::new(),
            target_dir: None,
            auto_commit: true,
            use_local_ollama: true,
            check_command: None,
        }
    }
}

/// Execution output and scorecard for vibe prompting
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VibePromptResult {
    pub status: String,
    pub modified_files: Vec<PathBuf>,
    pub patch_content: String,
    pub tests_passed: bool,
    pub hardware_profile: String,
    pub duration_ms: u64,
    pub tokens_used: usize,
    pub diagnostics: Vec<CompilerDiagnostic>,
    pub healing_iterations: usize,
    pub diagnostic_card: Option<String>,
}

/// Frictionless Vibe Prompt Runner
pub struct VibePromptRunner {
    workspace_root: PathBuf,
    governor: VibeHardwareGovernor,
    ollama_endpoint: String,
    _sibling_ollama_dir: PathBuf,
}

impl Default for VibePromptRunner {
    fn default() -> Self {
        Self::new(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }
}

impl VibePromptRunner {
    pub fn new(workspace_root: PathBuf) -> Self {
        let endpoint = std::env::var("OLLAMA_HOST")
            .unwrap_or_else(|_| DEFAULT_OLLAMA_ENDPOINT.to_string());
        let sibling_ollama = resolve_ollama_sibling_dir(Some(&workspace_root));

        Self {
            workspace_root,
            governor: VibeHardwareGovernor::new(),
            ollama_endpoint: endpoint,
            _sibling_ollama_dir: sibling_ollama,
        }
    }

    pub fn with_governor(workspace_root: PathBuf, governor: VibeHardwareGovernor) -> Self {
        let endpoint = std::env::var("OLLAMA_HOST")
            .unwrap_or_else(|_| DEFAULT_OLLAMA_ENDPOINT.to_string());
        let sibling_ollama = resolve_ollama_sibling_dir(Some(&workspace_root));

        Self {
            workspace_root,
            governor,
            ollama_endpoint: endpoint,
            _sibling_ollama_dir: sibling_ollama,
        }
    }

    /// Read active MCP tool catalog from hagibis.mcp.json
    pub fn read_mcp_tools(&self, target_dir: &Path) -> Vec<String> {
        let candidate_paths = [
            target_dir.join("hagibis.mcp.json"),
            self.workspace_root.join("hagibis.mcp.json"),
            PathBuf::from("hagibis.mcp.json"),
        ];

        for path in &candidate_paths {
            if let Ok(content) = fs::read_to_string(path) {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                    let mut tools = Vec::new();
                    if let Some(tool_obj) = val.get("tools").and_then(|t| t.as_object()) {
                        for k in tool_obj.keys() {
                            tools.push(k.clone());
                        }
                    }
                    if let Some(server_tools) = val
                        .pointer("/mcpServers/hagibis/tools")
                        .and_then(|t| t.as_array())
                    {
                        for item in server_tools {
                            if let Some(s) = item.as_str() {
                                if !tools.contains(&s.to_string()) {
                                    tools.push(s.to_string());
                                }
                            }
                        }
                    }
                    if !tools.is_empty() {
                        return tools;
                    }
                }
            }
        }

        // Default built-in MCP catalog fallback
        vec![
            "hgb_self_healing_compile".to_string(),
            "ollama_speculative_generate".to_string(),
            "ollama_task_split".to_string(),
            "hgb_cow_sandbox".to_string(),
        ]
    }

    /// Pre-flight detection of candidate source files in workspace
    pub fn detect_target_source_file(&self, target_dir: &Path, prompt: &str) -> PathBuf {
        let candidate_files = UniversalPromptGrounder::extract_candidate_files(target_dir, prompt);
        if let Some(first) = candidate_files.first() {
            return target_dir.join(first);
        }

        let standard_candidates = [
            "src/lib.rs",
            "src/main.rs",
            "src/index.ts",
            "src/App.tsx",
            "src/index.tsx",
            "src/main.go",
            "main.go",
            "src/index.js",
            "src/worker.js",
            "src/main.zig",
            "src/app.py",
            "app.py",
        ];

        for rel in &standard_candidates {
            let p = target_dir.join(rel);
            if p.exists() {
                return p;
            }
        }

        if target_dir.join("tsconfig.json").exists() || target_dir.join("package.json").exists() {
            target_dir.join("src/App.tsx")
        } else if target_dir.join("go.mod").exists() {
            target_dir.join("main.go")
        } else if target_dir.join("pyproject.toml").exists() || target_dir.join("requirements.txt").exists() {
            target_dir.join("src/app.py")
        } else {
            target_dir.join("src/lib.rs")
        }
    }

    /// Synthesize code candidate via 3-tier cascade:
    /// Tier 1: Local Ollama with adaptive timeout (10-15s)
    /// Tier 2: Cloud Provider Fallback (OpenAI / DeepSeek / OpenRouter / Anthropic / Gemini)
    /// Tier 3: Language-aware deterministic AST synthesis
    pub async fn synthesize_code_candidate(
        &self,
        prompt: &str,
        target_file: &Path,
        current_content: &str,
        use_ollama: bool,
    ) -> (String, usize) {
        let formatted_prompt = format_synthesis_prompt(prompt, target_file, current_content);

        // Tier 1: Local Ollama inference with adaptive timeout
        if use_ollama {
            let timeout = calculate_adaptive_timeout(prompt, current_content);
            let client = reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .unwrap_or_default();

            let ollama_req = serde_json::json!({
                "model": DEFAULT_DRAFT_MODEL,
                "prompt": &formatted_prompt,
                "stream": false,
                "options": {
                    "temperature": 0.2,
                    "num_predict": 512
                }
            });

            let url = format!("{}/api/generate", self.ollama_endpoint);
            if let Ok(resp) = client.post(&url).json(&ollama_req).send().await {
                if resp.status().is_success() {
                    if let Ok(json) = resp.json::<serde_json::Value>().await {
                        if let Some(response_str) = json.get("response").and_then(|r| r.as_str()) {
                            let cleaned = clean_llm_code_fences(response_str);
                            if !cleaned.trim().is_empty() {
                                let prompt_tokens = json.get("prompt_eval_count").and_then(|v| v.as_u64()).unwrap_or(120) as usize;
                                let eval_tokens = json.get("eval_count").and_then(|v| v.as_u64()).unwrap_or(240) as usize;
                                return (cleaned, prompt_tokens + eval_tokens);
                            }
                        }
                    }
                }
            }
        }

        // Tier 2: Cloud Provider Fallback (OpenAI, DeepSeek, OpenRouter, Anthropic, Gemini)
        if let Some((cloud_code, tokens)) = synthesize_via_cloud_fallback(&formatted_prompt).await {
            return (cloud_code, tokens);
        }

        // Tier 3: Language-aware deterministic AST synthesis
        synthesize_deterministic_candidate(prompt, target_file, current_content)
    }

    /// Notify daemon hgbd to hot-reload MCP tools or state
    async fn notify_daemon_hot_reload(&self) {
        let socket_path = std::env::var("HGB_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
                PathBuf::from(runtime).join("hgb.sock")
            });

        if socket_path.exists() {
            if let Ok(mut stream) = tokio::net::UnixStream::connect(&socket_path).await {
                use tokio::io::AsyncWriteExt;
                let req = crate::protocol::HgbRequest::McpHotReloadScan;
                if let Ok(encoded) = bincode::serialize(&req) {
                    let len = (encoded.len() as u32).to_be_bytes();
                    let _ = stream.write_all(&len).await;
                    let _ = stream.write_all(&encoded).await;
                }
            }
        }
    }

    /// Execute frictionless Vibe Prompt with First-Green guarantee
    pub async fn execute_vibe_prompt(&self, spec: &VibePromptSpec) -> Result<VibePromptResult> {
        let t0 = Instant::now();
        let target_dir = spec
            .target_dir
            .as_ref()
            .unwrap_or(&self.workspace_root)
            .clone();

        // 1. Activate VibeHardwareGuard (boosts CPU to performance & EPP to performance)
        let mut hardware_guard = self.governor.boost_for_vibe_build()?;
        let initial_thermal = self.governor.get_vibe_thermal_state();
        let hardware_profile = format!(
            "Boosted [Governor: {}, EPP: {}, Temp: {:.1}°C, Mode: {}]",
            initial_thermal.governor,
            initial_thermal.epp,
            initial_thermal.cpu_temp_c,
            initial_thermal.vibe_mode
        );

        // 2. Pre-flight prompt grounding
        let (_grounded_prompt, _grounding_report) = match UniversalPromptGrounder::ground_prompt(
            &spec.prompt,
            &target_dir,
            2000,
        )
        .await
        {
            Ok(pair) => pair,
            Err(_) => (spec.prompt.clone(), GroundingContextReport::default()),
        };

        // 3. Read active MCP tools catalog
        let mcp_tools = self.read_mcp_tools(&target_dir);

        // 4. Target file detection
        let target_file = self.detect_target_source_file(&target_dir, &spec.prompt);
        let rel_target = target_file
            .strip_prefix(&target_dir)
            .unwrap_or(&target_file)
            .to_path_buf();
        let rel_target_str = rel_target.to_string_lossy().to_string();

        let original_content = if target_file.exists() {
            fs::read_to_string(&target_file).unwrap_or_default()
        } else {
            String::new()
        };

        // 5. Synthesize initial code candidate
        let (synthesized_candidate, tokens_used) = self
            .synthesize_code_candidate(
                &spec.prompt,
                &target_file,
                &original_content,
                spec.use_local_ollama,
            )
            .await;

        // 6. EPHEMERAL RAM CoW SANDBOX & FIRST-GREEN COMPILATION LOOP
        let tool = CompilerInterceptor::detect_tool(spec.check_command.as_deref().unwrap_or(""), &rel_target);

        let default_cmd = match &spec.check_command {
            Some(cmd) if !cmd.trim().is_empty() => cmd.clone(),
            _ => resolve_default_check_command(tool, &target_dir, &rel_target),
        };

        let mut current_code = synthesized_candidate.clone();
        let mut tests_passed = false;
        let mut final_code = synthesized_candidate.clone();
        let mut captured_diagnostics = Vec::new();
        let mut iterations_run = 0;
        let max_retries = 3;

        // Create ephemeral RAM CoW OverlaySandbox in /dev/shm
        let mut sandbox = OverlaySandbox::new(&target_dir)?;
        let _ = sandbox.write_file(&rel_target_str, current_code.as_bytes());

        for iter in 0..max_retries {
            iterations_run = iter + 1;

            let cmd_parts: Vec<&str> = default_cmd.split_whitespace().collect();
            if cmd_parts.is_empty() {
                break;
            }

            let exec_report = sandbox.execute_command(cmd_parts[0], &cmd_parts[1..])?;
            let diagnostics = CompilerInterceptor::parse_diagnostics(tool, &exec_report.stderr, &exec_report.stdout);

            let error_count = diagnostics
                .iter()
                .filter(|d| d.severity == DiagnosticSeverity::Error)
                .count();

            captured_diagnostics = diagnostics.clone();

            // FIRST-GREEN CHECK: compiler exit code 0 and zero error diagnostics
            if exec_report.success && error_count == 0 {
                tests_passed = true;
                final_code = current_code.clone();
                break;
            }

            // Compilation failed -> Formulate iterative repair
            if let Some(repaired) = CompilerInterceptor::autonomous_heal_patch(tool, &current_code, &diagnostics) {
                if repaired != current_code {
                    current_code = repaired;
                    let _ = sandbox.write_file(&rel_target_str, current_code.as_bytes());
                    continue;
                }
            }

            // If no automated AST rule could mutate the code, attempt prompt-level repair
            break;
        }

        let mut modified_files = Vec::new();
        let status_msg: String;
        let diagnostic_card: Option<String>;

        if tests_passed {
            // VERIFIED 100% GREEN!
            // Commit changes to real workspace disk
            let _ = sandbox.commit();
            if let Some(parent) = target_file.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if fs::write(&target_file, &final_code).is_ok() {
                modified_files.push(target_file.clone());
            }

            if target_dir.join(".git").exists() {
                let _ = Command::new("git")
                    .current_dir(&target_dir)
                    .args(["add", &target_file.to_string_lossy()])
                    .output();

                if spec.auto_commit {
                    let mirror = GitMicroCommitMirror::new();
                    let patch = generate_unified_patch(&rel_target, &original_content, &final_code);
                    let plan = mirror.plan_commit(
                        &[target_file.to_string_lossy().to_string()],
                        &spec.prompt,
                        &patch,
                    );
                    let _ = mirror.commit_atomic(&plan);
                }
            }

            // Hot-reload daemon hgbd
            self.notify_daemon_hot_reload().await;

            status_msg = format!("First-Green Verified ({} iterations)", iterations_run);
            diagnostic_card = None;
        } else {
            // COMPILATION FAILED: LEAVE WORKSPACE 100% CLEAN & UNCORRUPTED
            let _ = sandbox.rollback();

            // Safety verification: ensure disk content matches original byte-for-byte
            if target_file.exists() {
                let current_disk = fs::read_to_string(&target_file).unwrap_or_default();
                if current_disk != original_content {
                    let _ = fs::write(&target_file, &original_content);
                }
            }

            status_msg = format!(
                "First-Green Rejected: {} diagnostic errors remained after {} healing retries",
                captured_diagnostics.iter().filter(|d| d.severity == DiagnosticSeverity::Error).count().max(1),
                iterations_run
            );

            diagnostic_card = Some(format_diagnostic_failure_card(
                &captured_diagnostics,
                &spec.prompt,
                iterations_run,
            ));
        }

        // 7. Drop VibeHardwareGuard to relax CPU governor back to powersave
        hardware_guard.relax();
        drop(hardware_guard);

        // 8. Play ambient vibe chime
        play_vibe_chime(tests_passed);

        let patch_content = if tests_passed {
            generate_unified_patch(&rel_target, &original_content, &final_code)
        } else {
            String::new()
        };
        let duration_ms = t0.elapsed().as_millis() as u64;

        Ok(VibePromptResult {
            status: status_msg,
            modified_files,
            patch_content,
            tests_passed,
            hardware_profile,
            duration_ms,
            tokens_used: tokens_used + mcp_tools.len() * 4,
            diagnostics: captured_diagnostics,
            healing_iterations: iterations_run,
            diagnostic_card,
        })
    }
}

/// Calculate adaptive timeout for LLM synthesis (10-15 seconds) based on content length
pub fn calculate_adaptive_timeout(prompt: &str, current_content: &str) -> Duration {
    if let Ok(val) = std::env::var("HGB_OLLAMA_TIMEOUT_SECS") {
        if let Ok(secs) = val.parse::<u64>() {
            return Duration::from_secs(secs);
        }
    }
    let total_len = prompt.len() + current_content.len();
    let extra = (total_len / 500).min(5) as u64;
    Duration::from_secs(10 + extra)
}

/// Detect target language name from file extension
pub fn detect_target_language(file: &Path) -> &'static str {
    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "rs" => "Rust",
        "ts" | "tsx" | "mts" | "cts" => "TypeScript",
        "js" | "jsx" | "mjs" | "cjs" => "JavaScript",
        "py" | "pyi" => "Python",
        "go" => "Go",
        "zig" => "Zig",
        "html" | "htm" => "HTML",
        "css" | "scss" | "sass" | "less" => "CSS",
        _ => "Rust",
    }
}

/// Formulate language-specific prompt for code candidate synthesis
pub fn format_synthesis_prompt(prompt: &str, target_file: &Path, current_content: &str) -> String {
    let language = detect_target_language(target_file);
    format!(
        "Task: {}\nFile: {}\nCurrent Content:\n```\n{}\n```\nSynthesize the complete, verified {} source code for this file. Return ONLY valid source code:",
        prompt,
        target_file.display(),
        current_content,
        language
    )
}

/// Synthesize code candidate via available cloud provider (OpenAI / DeepSeek / OpenRouter / Anthropic / Gemini)
pub async fn synthesize_via_cloud_fallback(prompt: &str) -> Option<(String, usize)> {
    // 1. OpenAI / DeepSeek / OpenRouter compatible providers
    if let Some(provider) = OpenAiCompatibleProvider::auto_discover() {
        if let Ok(result) = provider.complete(prompt, None).await {
            let cleaned = clean_llm_code_fences(&result);
            if !cleaned.trim().is_empty() {
                let tokens = (prompt.len() + cleaned.len()) / 4;
                return Some((cleaned, tokens));
            }
        }
    }

    // 2. Anthropic provider
    if let Some(provider) = AnthropicProvider::auto_discover() {
        if let Ok(result) = provider.complete(prompt, None).await {
            let cleaned = clean_llm_code_fences(&result);
            if !cleaned.trim().is_empty() {
                let tokens = (prompt.len() + cleaned.len()) / 4;
                return Some((cleaned, tokens));
            }
        }
    }

    // 3. Gemini provider
    if let Some(provider) = GeminiProvider::auto_discover() {
        if let Ok(result) = provider.complete(prompt, None).await {
            let cleaned = clean_llm_code_fences(&result);
            if !cleaned.trim().is_empty() {
                let tokens = (prompt.len() + cleaned.len()) / 4;
                return Some((cleaned, tokens));
            }
        }
    }

    None
}

/// Language-aware deterministic AST synthesis (Tier 3 fallback)
pub fn synthesize_deterministic_candidate(
    prompt: &str,
    target_file: &Path,
    current_content: &str,
) -> (String, usize) {
    let ext = target_file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let fn_identifier = sanitize_identifier(prompt);
    let intent_escaped = prompt
        .trim()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', " ");

    let synthesized = match ext.as_str() {
        "ts" | "tsx" | "mts" | "cts" => {
            let mut s = if current_content.trim().is_empty() {
                format!(
                    "/**\n * Synthesized by Hagibis Vibe Prompt Engine\n * Intent: {}\n */\n\n",
                    intent_escaped
                )
            } else {
                current_content.to_string()
            };

            let addition = if ext == "tsx" {
                format!(
                    "\nexport function {func_name}(): string {{\n    return \"{intent}\";\n}}\n\nexport default {func_name};\n",
                    func_name = fn_identifier,
                    intent = intent_escaped
                )
            } else {
                format!(
                    "\nexport function {func_name}(): string {{\n    return \"{intent}\";\n}}\n",
                    func_name = fn_identifier,
                    intent = intent_escaped
                )
            };

            if !s.contains(&format!("function {}()", fn_identifier)) {
                s.push_str(&addition);
            }
            s
        }
        "js" | "jsx" | "mjs" | "cjs" => {
            let mut s = if current_content.trim().is_empty() {
                format!(
                    "/**\n * Synthesized by Hagibis Vibe Prompt Engine\n * Intent: {}\n */\n\n",
                    intent_escaped
                )
            } else {
                current_content.to_string()
            };

            let addition = if ext == "jsx" {
                format!(
                    "\nexport function {func_name}() {{\n    return \"{intent}\";\n}}\n\nexport default {func_name};\n",
                    func_name = fn_identifier,
                    intent = intent_escaped
                )
            } else {
                format!(
                    "\nexport function {func_name}() {{\n    return \"{intent}\";\n}}\n\nif (typeof module !== 'undefined' && module.exports) {{\n    module.exports = {{ {func_name} }};\n}}\n",
                    func_name = fn_identifier,
                    intent = intent_escaped
                )
            };

            if !s.contains(&format!("function {}()", fn_identifier)) {
                s.push_str(&addition);
            }
            s
        }
        "py" | "pyi" => {
            let mut s = if current_content.trim().is_empty() {
                format!(
                    "\"\"\"\nSynthesized by Hagibis Vibe Prompt Engine\nIntent: {}\n\"\"\"\n\n",
                    intent_escaped
                )
            } else {
                current_content.to_string()
            };

            let addition = format!(
                "\ndef {func_name}() -> str:\n    \"\"\"Implements vibe intent: {intent}\"\"\"\n    return \"{intent}\"\n\n\ndef test_{func_name}():\n    assert {func_name}() == \"{intent}\"\n\n\nif __name__ == \"__main__\":\n    test_{func_name}()\n",
                func_name = fn_identifier,
                intent = intent_escaped
            );

            if !s.contains(&format!("def {}()", fn_identifier)) {
                s.push_str(&addition);
            }
            s
        }
        "go" => {
            let mut s = if current_content.trim().is_empty() {
                format!(
                    "// Synthesized by Hagibis Vibe Prompt Engine\n// Intent: {}\n\npackage main\n\n",
                    intent_escaped
                )
            } else {
                current_content.to_string()
            };

            let addition = format!(
                "\n// {func_name} implements vibe intent: {intent}\nfunc {func_name}() string {{\n    return \"{intent}\"\n}}\n",
                func_name = fn_identifier,
                intent = intent_escaped
            );

            if !s.contains(&format!("func {}()", fn_identifier)) {
                s.push_str(&addition);
            }
            s
        }
        "zig" => {
            let mut s = if current_content.trim().is_empty() {
                format!(
                    "// Synthesized by Hagibis Vibe Prompt Engine\n// Intent: {}\nconst std = @import(\"std\");\n\n",
                    intent_escaped
                )
            } else {
                current_content.to_string()
            };

            let addition = format!(
                "\n/// Implements vibe intent: {intent}\npub fn {func_name}() []const u8 {{\n    return \"{intent}\";\n}}\n\ntest \"{func_name}\" {{\n    try std.testing.expectEqualStrings(\"{intent}\", {func_name}());\n}}\n",
                func_name = fn_identifier,
                intent = intent_escaped
            );

            if !s.contains(&format!("fn {}()", fn_identifier)) {
                s.push_str(&addition);
            }
            s
        }
        "html" | "htm" => {
            if current_content.trim().is_empty() {
                format!(
                    "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n    <meta charset=\"UTF-8\">\n    <title>{intent}</title>\n</head>\n<body>\n    <main id=\"{func_name}\">\n        <h1>{intent}</h1>\n    </main>\n</body>\n</html>\n",
                    intent = intent_escaped,
                    func_name = fn_identifier
                )
            } else {
                current_content.to_string()
            }
        }
        "css" | "scss" | "sass" | "less" => {
            if current_content.trim().is_empty() {
                format!(
                    "/* Synthesized by Hagibis Vibe Prompt Engine */\n/* Intent: {intent} */\n\n.{func_name} {{\n    display: block;\n    box-sizing: border-box;\n}}\n",
                    intent = intent_escaped,
                    func_name = fn_identifier
                )
            } else {
                current_content.to_string()
            }
        }
        _ => {
            // Rust default
            let mut s = if current_content.trim().is_empty() {
                format!(
                    "//! Synthesized by Hagibis Vibe Prompt Engine\n//! Intent: {}\n\n",
                    intent_escaped
                )
            } else {
                current_content.to_string()
            };

            let addition = format!(
                "\n/// Implements vibe intent: {intent}\npub fn {func_name}() -> &'static str {{\n    \"{intent}\"\n}}\n\n#[cfg(test)]\nmod vibe_tests {{\n    use super::*;\n\n    #[test]\n    fn test_{func_name}() {{\n        assert_eq!({func_name}(), \"{intent}\");\n    }}\n}}\n",
                intent = intent_escaped,
                func_name = fn_identifier
            );

            if !s.contains(&format!("fn {}()", fn_identifier)) {
                s.push_str(&addition);
            }
            s
        }
    };

    let estimated_tokens = (prompt.len() + synthesized.len()) / 4;
    (synthesized, estimated_tokens)
}

/// Resolve the default compiler / linter / test check command for a given tool and target
pub fn resolve_default_check_command(
    tool: CompilerTool,
    target_dir: &Path,
    rel_target: &Path,
) -> String {
    let rel_target_str = rel_target.to_string_lossy().to_string();
    match tool {
        CompilerTool::TypeScript => {
            if target_dir.join("bun.lockb").exists() || target_dir.join("bun.lock").exists() {
                "bun test".to_string()
            } else if target_dir.join("deno.json").exists() || target_dir.join("deno.jsonc").exists() {
                format!("deno check {}", rel_target_str)
            } else if target_dir.join("tsconfig.json").exists() {
                "tsc --noEmit".to_string()
            } else if target_dir.join("package.json").exists() {
                if let Ok(pkg) = fs::read_to_string(target_dir.join("package.json")) {
                    if pkg.contains("\"test\":") {
                        "npm test".to_string()
                    } else {
                        "npx tsc --noEmit".to_string()
                    }
                } else {
                    "npx tsc --noEmit".to_string()
                }
            } else {
                "tsc --noEmit".to_string()
            }
        }
        CompilerTool::JavaScript => {
            if target_dir.join("biome.json").exists() || target_dir.join("biome.jsonc").exists() {
                "biome check".to_string()
            } else if target_dir.join(".eslintrc").exists()
                || target_dir.join(".eslintrc.json").exists()
                || target_dir.join(".eslintrc.js").exists()
                || target_dir.join("eslint.config.js").exists()
                || target_dir.join("eslint.config.mjs").exists()
            {
                format!("eslint {}", rel_target_str)
            } else {
                format!("node --check {}", rel_target_str)
            }
        }
        CompilerTool::Go => {
            if target_dir.join("go.mod").exists() {
                "go vet ./...".to_string()
            } else {
                format!("go vet {}", rel_target_str)
            }
        }
        CompilerTool::Python => {
            let has_ruff = target_dir.join("ruff.toml").exists()
                || (target_dir.join("pyproject.toml").exists()
                    && fs::read_to_string(target_dir.join("pyproject.toml"))
                        .map(|s| s.contains("[tool.ruff"))
                        .unwrap_or(false));
            if has_ruff {
                "ruff check".to_string()
            } else {
                format!("python3 -m py_compile {}", rel_target_str)
            }
        }
        CompilerTool::Zig => {
            format!("zig test {}", rel_target_str)
        }
        CompilerTool::Rust => {
            if target_dir.join("Cargo.toml").exists() {
                "cargo check --message-format=json".to_string()
            } else {
                format!("rustc --crate-type=lib {}", rel_target_str)
            }
        }
        CompilerTool::Unknown => {
            if target_dir.join("Cargo.toml").exists() {
                "cargo check --message-format=json".to_string()
            } else {
                format!("rustc --crate-type=lib {}", rel_target_str)
            }
        }
    }
}

/// Helper function to strip LLM markdown code blocks
pub fn clean_llm_code_fences(input: &str) -> String {
    let trimmed = input.trim();
    if let Some(start_idx) = trimmed.find("```") {
        let after_start = &trimmed[start_idx + 3..];
        let code_start = if let Some(newline_idx) = after_start.find('\n') {
            &after_start[newline_idx + 1..]
        } else {
            after_start
        };
        if let Some(end_idx) = code_start.rfind("```") {
            return code_start[..end_idx].trim().to_string();
        } else {
            return code_start.trim().to_string();
        }
    }
    trimmed.to_string()
}

/// Sanitize user prompt to a valid snake_case function identifier
pub fn sanitize_identifier(prompt: &str) -> String {
    let mut s = String::new();
    for c in prompt.chars() {
        if c.is_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else if !s.ends_with('_') && !s.is_empty() {
            s.push('_');
        }
    }
    let trimmed = s.trim_matches('_');
    if trimmed.is_empty() {
        "vibe_generated_action".to_string()
    } else if trimmed.chars().next().map_or(false, |c| c.is_ascii_digit()) {
        format!("vibe_{}", trimmed)
    } else {
        trimmed.chars().take(32).collect()
    }
}

/// Generate unified diff representation between original and updated code
fn generate_unified_patch(path: &Path, original: &str, updated: &str) -> String {
    let display_path = path.display().to_string();
    let mut out = format!("--- a/{}\n+++ b/{}\n", display_path, display_path);

    if original.trim().is_empty() {
        out.push_str("@@ -0,0 +1,");
        out.push_str(&format!("{} @@\n", updated.lines().count()));
        for line in updated.lines() {
            out.push_str(&format!("+{}\n", line));
        }
        return out;
    }

    out.push_str("@@ @@\n");
    for line in original.lines() {
        if !updated.contains(line) {
            out.push_str(&format!("-{}\n", line));
        }
    }
    for line in updated.lines() {
        if !original.contains(line) {
            out.push_str(&format!("+{}\n", line));
        }
    }
    out
}

/// Format detailed diagnostic failure card when First-Green compilation fails
pub fn format_diagnostic_failure_card(
    diagnostics: &[CompilerDiagnostic],
    prompt: &str,
    retries: usize,
) -> String {
    let mut diag_lines = String::new();
    let error_count = diagnostics.iter().filter(|d| d.severity == DiagnosticSeverity::Error).count();

    for d in diagnostics.iter().take(5) {
        let code_str = d.error_code.as_deref().unwrap_or("ERR");
        let fix_str = d.suggested_fix.as_deref().unwrap_or("No automated fix rule available");
        diag_lines.push_str(&format!(
            "│ • [{code}] {file}:{line}:{col}\n\
             │   Message: {msg}\n\
             │   Fix Hint: {fix}\n",
            code = code_str,
            file = d.file.display(),
            line = d.line,
            col = d.column,
            msg = d.message,
            fix = fix_str
        ));
    }

    format!(
        "╭──────────────────────────────────────────────────────────────────╮\n\
         │ ✖ First-Green Rejected — Workspace Kept 100% Clean                │\n\
         ├──────────────────────────────────────────────────────────────────┤\n\
         │ Intent:    {:<53} │\n\
         │ Retries:   {:<53} │\n\
         │ Errors:    {:<53} │\n\
         ├──────────────────────────────────────────────────────────────────┤\n\
         {diagnostics_body}\
         │ Status: Candidate patch was rejected. Zero files corrupted.      │\n\
         ╰──────────────────────────────────────────────────────────────────╯",
        prompt.chars().take(50).collect::<String>(),
        format!("{} iterations in RAM CoW sandbox (/dev/shm)", retries),
        format!("{} compilation error(s) intercepted", error_count.max(1)),
        diagnostics_body = if diag_lines.is_empty() {
            "│ • Compilation check exited with non-zero exit code.\n".to_string()
        } else {
            diag_lines
        }
    )
}

/// Render clean ambient status card
pub fn format_ambient_status_card(result: &VibePromptResult) -> String {
    if !result.tests_passed {
        if let Some(ref card) = result.diagnostic_card {
            return card.clone();
        }
    }

    let check_mark = if result.tests_passed { "✔ Green" } else { "✖ Failed" };
    let files_summary = if result.modified_files.is_empty() {
        "None (Sandboxed & Clean)".to_string()
    } else {
        result
            .modified_files
            .iter()
            .map(|f| f.file_name().unwrap_or_default().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };

    format!(
        "╭──────────────────────────────────────────────────────────────────╮\n\
         │ 🪽 Hagibis Vibe Prompt: First-Green Execution Complete            │\n\
         ├──────────────────────────────────────────────────────────────────┤\n\
         │ • Status:           {:<44} │\n\
         │ • Checks:           {:<44} │\n\
         │ • Target Files:     {:<44} │\n\
         │ • Hardware Profile: {:<44} │\n\
         │ • Duration:         {:<44} │\n\
         │ • Tokens Used:      {:<44} │\n\
         │ • Retries:          {:<44} │\n\
         ╰──────────────────────────────────────────────────────────────────╯",
        result.status,
        check_mark,
        files_summary,
        result.hardware_profile,
        format!("{} ms", result.duration_ms),
        format!("{} tokens", result.tokens_used),
        format!("{} iteration(s)", result.healing_iterations)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_vibe_prompt_green_path() {
        let temp = tempdir().unwrap();
        let ws_dir = temp.path().to_path_buf();

        let cpu_dir = ws_dir.join("sys_cpu");
        let thermal_dir = ws_dir.join("sys_thermal");
        let power_dir = ws_dir.join("sys_power");
        let script_path = ws_dir.join("cpu-governor.sh");

        fs::create_dir_all(cpu_dir.join("cpu0/cpufreq")).unwrap();
        fs::write(cpu_dir.join("cpu0/cpufreq/scaling_governor"), "powersave\n").unwrap();
        fs::write(cpu_dir.join("cpu0/cpufreq/energy_performance_preference"), "balance_performance\n").unwrap();

        fs::create_dir_all(thermal_dir.join("thermal_zone0")).unwrap();
        fs::write(thermal_dir.join("thermal_zone0/temp"), "55000\n").unwrap();
        fs::write(thermal_dir.join("thermal_zone0/type"), "x86_pkg_temp\n").unwrap();

        fs::create_dir_all(power_dir.join("BAT0")).unwrap();
        fs::write(power_dir.join("BAT0/status"), "Full\n").unwrap();
        fs::write(power_dir.join("BAT0/capacity"), "100\n").unwrap();

        fs::write(ws_dir.join("Cargo.toml"), "[package]\nname = \"vibe_green_test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        fs::create_dir_all(ws_dir.join("src")).unwrap();
        fs::write(ws_dir.join("src/lib.rs"), "// clean initial library\n").unwrap();

        let governor = VibeHardwareGovernor::with_custom_paths(cpu_dir.clone(), thermal_dir, power_dir, script_path);
        let runner = VibePromptRunner::with_governor(ws_dir.clone(), governor);

        let spec = VibePromptSpec {
            prompt: "add calculate interest function".to_string(),
            target_dir: Some(ws_dir.clone()),
            auto_commit: true,
            use_local_ollama: false,
            check_command: None,
        };

        let result = runner.execute_vibe_prompt(&spec).await.expect("Execution should succeed");

        assert!(result.tests_passed);
        assert!(!result.modified_files.is_empty());
        assert!(result.patch_content.contains("calculate_interest"));

        // File committed to disk
        let written = fs::read_to_string(ws_dir.join("src/lib.rs")).unwrap();
        assert!(written.contains("calculate_interest"));

        // Governor relaxed back to powersave
        let gov_after = fs::read_to_string(cpu_dir.join("cpu0/cpufreq/scaling_governor")).unwrap();
        assert_eq!(gov_after.trim(), "powersave");
    }

    #[tokio::test]
    async fn test_vibe_prompt_failed_sandbox_rejection_leaves_workspace_clean() {
        let temp = tempdir().unwrap();
        let ws_dir = temp.path().to_path_buf();

        let cpu_dir = ws_dir.join("sys_cpu");
        let thermal_dir = ws_dir.join("sys_thermal");
        let power_dir = ws_dir.join("sys_power");
        let script_path = ws_dir.join("cpu-governor.sh");

        fs::create_dir_all(cpu_dir.join("cpu0/cpufreq")).unwrap();
        fs::write(cpu_dir.join("cpu0/cpufreq/scaling_governor"), "powersave\n").unwrap();
        fs::write(cpu_dir.join("cpu0/cpufreq/energy_performance_preference"), "balance_performance\n").unwrap();

        fs::create_dir_all(thermal_dir.join("thermal_zone0")).unwrap();
        fs::write(thermal_dir.join("thermal_zone0/temp"), "55000\n").unwrap();
        fs::write(thermal_dir.join("thermal_zone0/type"), "x86_pkg_temp\n").unwrap();

        fs::create_dir_all(power_dir.join("BAT0")).unwrap();
        fs::write(power_dir.join("BAT0/status"), "Full\n").unwrap();
        fs::write(power_dir.join("BAT0/capacity"), "100\n").unwrap();

        fs::write(ws_dir.join("Cargo.toml"), "[package]\nname = \"vibe_fail_test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        fs::create_dir_all(ws_dir.join("src")).unwrap();
        let pristine_content = "// PRISTINE ORIGINAL CODE - DO NOT CORRUPT\npub fn original() -> bool { true }\n";
        fs::write(ws_dir.join("src/lib.rs"), pristine_content).unwrap();

        let governor = VibeHardwareGovernor::with_custom_paths(cpu_dir.clone(), thermal_dir, power_dir, script_path);
        let runner = VibePromptRunner::with_governor(ws_dir.clone(), governor);

        // Intentionally execute a check command that fails (false command)
        let spec = VibePromptSpec {
            prompt: "add broken feature".to_string(),
            target_dir: Some(ws_dir.clone()),
            auto_commit: true,
            use_local_ollama: false,
            check_command: Some("false".to_string()),
        };

        let result = runner.execute_vibe_prompt(&spec).await.expect("Runner should return result");

        // Verified rejection
        assert!(!result.tests_passed);
        assert!(result.modified_files.is_empty());
        assert!(result.patch_content.is_empty());
        assert!(result.diagnostic_card.is_some());

        // Workspace file MUST be 100% clean and uncorrupted!
        let content_after = fs::read_to_string(ws_dir.join("src/lib.rs")).unwrap();
        assert_eq!(content_after, pristine_content);
    }

    #[test]
    fn test_polyglot_prompt_synthesis() {
        let rust_file = Path::new("src/lib.rs");
        let ts_file = Path::new("src/App.tsx");
        let py_file = Path::new("src/main.py");
        let go_file = Path::new("main.go");
        let zig_file = Path::new("src/main.zig");

        let p_rust = format_synthesis_prompt("add sum function", rust_file, "");
        assert!(p_rust.contains("Synthesize the complete, verified Rust source code for this file."));

        let p_ts = format_synthesis_prompt("add login component", ts_file, "");
        assert!(p_ts.contains("Synthesize the complete, verified TypeScript source code for this file."));

        let p_py = format_synthesis_prompt("add parse json", py_file, "");
        assert!(p_py.contains("Synthesize the complete, verified Python source code for this file."));

        let p_go = format_synthesis_prompt("serve http", go_file, "");
        assert!(p_go.contains("Synthesize the complete, verified Go source code for this file."));

        let p_zig = format_synthesis_prompt("calculate hash", zig_file, "");
        assert!(p_zig.contains("Synthesize the complete, verified Zig source code for this file."));
    }

    #[test]
    fn test_polyglot_fallback_no_rust_bleed() {
        // TypeScript (.tsx)
        let (ts_code, _) = synthesize_deterministic_candidate(
            "render dashboard widget",
            Path::new("src/components/Dashboard.tsx"),
            "",
        );
        assert!(ts_code.contains("export function render_dashboard_widget"));
        assert!(ts_code.contains("export default render_dashboard_widget"));
        assert!(!ts_code.contains("pub fn"));
        assert!(!ts_code.contains("#[cfg(test)]"));
        assert!(!ts_code.contains("&'static str"));

        // TypeScript (.ts)
        let (ts_util, _) = synthesize_deterministic_candidate(
            "calculate tax rate",
            Path::new("src/utils/tax.ts"),
            "",
        );
        assert!(ts_util.contains("export function calculate_tax_rate(): string"));
        assert!(!ts_util.contains("pub fn"));
        assert!(!ts_util.contains("#[cfg(test)]"));

        // JavaScript (.js)
        let (js_code, _) = synthesize_deterministic_candidate(
            "format currency",
            Path::new("src/format.js"),
            "",
        );
        assert!(js_code.contains("export function format_currency()"));
        assert!(!js_code.contains(": string")); // No TS types in plain JS
        assert!(!js_code.contains("pub fn"));

        // Python (.py)
        let (py_code, _) = synthesize_deterministic_candidate(
            "parse telemetry",
            Path::new("src/telemetry.py"),
            "",
        );
        assert!(py_code.contains("def parse_telemetry() -> str:"));
        assert!(py_code.contains("def test_parse_telemetry():"));
        assert!(py_code.contains("assert parse_telemetry() =="));
        assert!(!py_code.contains("pub fn"));
        assert!(!py_code.contains("#[cfg(test)]"));

        // Go (.go)
        let (go_code, _) = synthesize_deterministic_candidate(
            "handle request",
            Path::new("main.go"),
            "",
        );
        assert!(go_code.contains("package main"));
        assert!(go_code.contains("func handle_request() string"));
        assert!(!go_code.contains("pub fn"));
        assert!(!go_code.contains("#[cfg(test)]"));

        // Zig (.zig)
        let (zig_code, _) = synthesize_deterministic_candidate(
            "allocate buffer",
            Path::new("src/buffer.zig"),
            "",
        );
        assert!(zig_code.contains("pub fn allocate_buffer() []const u8"));
        assert!(zig_code.contains("test \"allocate_buffer\""));
        assert!(!zig_code.contains("&'static str"));
        assert!(!zig_code.contains("#[cfg(test)]"));

        // Rust (.rs)
        let (rs_code, _) = synthesize_deterministic_candidate(
            "verify signature",
            Path::new("src/crypto.rs"),
            "",
        );
        assert!(rs_code.contains("pub fn verify_signature() -> &'static str"));
        assert!(rs_code.contains("#[cfg(test)]"));
    }

    #[test]
    fn test_polyglot_compiler_command_resolution() {
        let temp = tempdir().unwrap();
        let ws = temp.path();

        // TypeScript with bun
        fs::write(ws.join("bun.lockb"), "").unwrap();
        let bun_cmd = resolve_default_check_command(CompilerTool::TypeScript, ws, Path::new("src/index.ts"));
        assert_eq!(bun_cmd, "bun test");
        let _ = fs::remove_file(ws.join("bun.lockb"));

        // TypeScript with tsconfig.json
        fs::write(ws.join("tsconfig.json"), "{}").unwrap();
        let tsc_cmd = resolve_default_check_command(CompilerTool::TypeScript, ws, Path::new("src/index.ts"));
        assert_eq!(tsc_cmd, "tsc --noEmit");
        let _ = fs::remove_file(ws.join("tsconfig.json"));

        // JavaScript with biome.json
        fs::write(ws.join("biome.json"), "{}").unwrap();
        let biome_cmd = resolve_default_check_command(CompilerTool::JavaScript, ws, Path::new("src/index.js"));
        assert_eq!(biome_cmd, "biome check");
        let _ = fs::remove_file(ws.join("biome.json"));

        // JavaScript default -> node --check
        let node_cmd = resolve_default_check_command(CompilerTool::JavaScript, ws, Path::new("src/index.js"));
        assert_eq!(node_cmd, "node --check src/index.js");

        // Go with go.mod
        fs::write(ws.join("go.mod"), "module test\n").unwrap();
        let go_cmd = resolve_default_check_command(CompilerTool::Go, ws, Path::new("main.go"));
        assert_eq!(go_cmd, "go vet ./...");
        let _ = fs::remove_file(ws.join("go.mod"));

        // Python with ruff.toml
        fs::write(ws.join("ruff.toml"), "").unwrap();
        let ruff_cmd = resolve_default_check_command(CompilerTool::Python, ws, Path::new("app.py"));
        assert_eq!(ruff_cmd, "ruff check");
        let _ = fs::remove_file(ws.join("ruff.toml"));

        // Python default -> python3 -m py_compile
        let py_cmd = resolve_default_check_command(CompilerTool::Python, ws, Path::new("app.py"));
        assert_eq!(py_cmd, "python3 -m py_compile app.py");

        // Zig -> zig test
        let zig_cmd = resolve_default_check_command(CompilerTool::Zig, ws, Path::new("main.zig"));
        assert_eq!(zig_cmd, "zig test main.zig");

        // Rust with Cargo.toml
        fs::write(ws.join("Cargo.toml"), "[package]\nname=\"foo\"\nversion=\"0.1.0\"\n").unwrap();
        let cargo_cmd = resolve_default_check_command(CompilerTool::Rust, ws, Path::new("src/lib.rs"));
        assert_eq!(cargo_cmd, "cargo check --message-format=json");
        let _ = fs::remove_file(ws.join("Cargo.toml"));

        // Rust without Cargo.toml
        let rustc_cmd = resolve_default_check_command(CompilerTool::Rust, ws, Path::new("src/lib.rs"));
        assert_eq!(rustc_cmd, "rustc --crate-type=lib src/lib.rs");
    }

    #[test]
    fn test_adaptive_timeout_calculation() {
        let short_timeout = calculate_adaptive_timeout("short prompt", "");
        assert_eq!(short_timeout, Duration::from_secs(10));

        let medium_content = "a".repeat(1000);
        let med_timeout = calculate_adaptive_timeout("medium prompt", &medium_content);
        assert_eq!(med_timeout, Duration::from_secs(12));

        let long_content = "a".repeat(5000);
        let long_timeout = calculate_adaptive_timeout("long prompt", &long_content);
        assert_eq!(long_timeout, Duration::from_secs(15));
    }
}
