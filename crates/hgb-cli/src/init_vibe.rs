//! Streamlined 1-Click Onboarding (`hgb init --vibe`)
//!
//! Rapid 1-click workspace onboarding for frictionless Vibe Coding:
//! 1. Initializes directory scaffolding (`.hgb/`, `.hgb/patches/`, `.hgb/timelines/`, `.hgb/mcp_plugins/`, `.hgb/distillation/`, `src/`)
//! 2. Writes standard `wrangler.toml` configured for Cloudflare Workers edge deployment with KV namespaces and Durable Objects
//! 3. Copies or links standard `hagibis.mcp.json` with all MCP tools and SSE endpoints
//! 4. Initializes Git repo if not present (`git init`)
//! 5. Verifies or starts `hgbd` daemon (`systemctl --user start hgbd`)
//! 6. Displays clean, stylized ASCII onboarding dashboard with next steps

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use colored::Colorize;
use serde::{Deserialize, Serialize};

use hgb_core::Result;

pub const DEFAULT_WRANGLER_TOML: &str = r#"name = "hagibis-edge-worker"
main = "src/index.js"
compatibility_date = "2026-10-01"

[vars]
ENVIRONMENT = "production"
DEPLOYED_BY = "hagibis-vibe-onboarding"

[[kv_namespaces]]
binding = "HGB_KV_STORAGE"
id = "hgb_kv_namespace_default"

[[durable_objects.bindings]]
name = "HGB_DURABLE_SESSION"
class_name = "HagibisSessionDurableObject"

[[migrations]]
tag = "v1"
new_classes = ["HagibisSessionDurableObject"]
"#;

pub const DEFAULT_MCP_JSON: &str = r#"{
  "tools": {
    "hgb_self_healing_compile": {
      "description": "Autonomous First-Green compilation loop: executes build/checks in RAM CoW overlay sandbox, intercepts diagnostics, and heals AST syntax errors before disk commit",
      "parameters": {
        "target_file": { "type": "string", "description": "Relative path to target source file", "required": true },
        "code": { "type": "string", "description": "Candidate source code string to verify and heal", "required": true },
        "command": { "type": "string", "description": "Compiler check command (e.g. cargo check, zig test, python3 -m py_compile)" },
        "max_retries": { "type": "integer", "description": "Maximum autonomous repair iterations", "default": 3 }
      }
    },
    "ollama_speculative_generate": {
      "description": "Speculative Decoding LLM inference using fast draft model (qwen2.5-coder:1.5b) validated by sovereign target model (phi3:3.8b) with zero-failure fallback",
      "parameters": {
        "prompt": { "type": "string", "description": "Prompt or instruction for speculative synthesis", "required": true },
        "draft_model": { "type": "string", "description": "Optional fast draft model (default: qwen2.5-coder:1.5b)" },
        "target_model": { "type": "string", "description": "Optional heavy target model (default: phi3:3.8b)" }
      }
    },
    "ollama_task_split": {
      "description": "Autonomous task complexity evaluator routing prompts between sub-second draft models (1.5B) and deep reasoning models (3.8B/7B)",
      "parameters": {
        "prompt": { "type": "string", "description": "Prompt or code task to evaluate", "required": true }
      }
    }
  },
  "mcpServers": {
    "hagibis": {
      "command": "hgb",
      "args": ["mcp"],
      "env": {
        "HGB_WORKSPACE": ".",
        "OLLAMA_HOST": "http://127.0.0.1:11434"
      },
      "tools": [
        "hgb_self_healing_compile",
        "ollama_speculative_generate",
        "ollama_task_split",
        "hgb_cow_sandbox"
      ]
    },
    "hagibis_sse": {
      "url": "http://127.0.0.1:8083/sse"
    }
  }
}
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VibeOnboardingReport {
    pub workspace_root: PathBuf,
    pub created_directories: Vec<PathBuf>,
    pub wrangler_toml_written: bool,
    pub mcp_json_written: bool,
    pub git_initialized: bool,
    pub daemon_running: bool,
    pub daemon_action_taken: String,
}

pub struct VibeOnboardingEngine;

impl VibeOnboardingEngine {
    /// Execute 1-click frictionless vibe onboarding
    pub fn execute_onboarding(target_dir_opt: Option<&Path>) -> Result<VibeOnboardingReport> {
        let ws = match target_dir_opt {
            Some(p) => {
                if !p.exists() {
                    fs::create_dir_all(p)?;
                }
                p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
            }
            None => std::env::current_dir()?,
        };

        let mut created_directories = Vec::new();

        // 1. Directory scaffolding
        let required_dirs = [
            ".hgb",
            ".hgb/patches",
            ".hgb/timelines",
            ".hgb/mcp_plugins",
            ".hgb/distillation",
            "src",
        ];

        for d in &required_dirs {
            let full_p = ws.join(d);
            if !full_p.exists() {
                fs::create_dir_all(&full_p)?;
                created_directories.push(full_p);
            }
        }

        // 2. Write wrangler.toml
        let wrangler_path = ws.join("wrangler.toml");
        let wrangler_toml_written = if !wrangler_path.exists() {
            fs::write(&wrangler_path, DEFAULT_WRANGLER_TOML)?;
            true
        } else {
            false
        };

        // 3. Copies or links standard hagibis.mcp.json
        let mcp_path = ws.join("hagibis.mcp.json");
        let mcp_json_written = if !mcp_path.exists() {
            // Check for sibling or root template
            let root_template = PathBuf::from("hagibis.mcp.json");
            if root_template.exists() && root_template != mcp_path {
                let _ = fs::copy(&root_template, &mcp_path);
            } else {
                fs::write(&mcp_path, DEFAULT_MCP_JSON)?;
            }
            true
        } else {
            false
        };

        // 4. Git repo initialization
        let git_dir = ws.join(".git");
        let git_initialized = if !git_dir.exists() {
            let res = Command::new("git")
                .arg("init")
                .current_dir(&ws)
                .output();
            res.map(|o| o.status.success()).unwrap_or(false)
        } else {
            false
        };

        // 5. Verify / start hgbd daemon
        let (daemon_running, daemon_action_taken) = Self::verify_or_start_daemon();

        Ok(VibeOnboardingReport {
            workspace_root: ws,
            created_directories,
            wrangler_toml_written,
            mcp_json_written,
            git_initialized,
            daemon_running,
            daemon_action_taken,
        })
    }

    /// Check daemon status and attempt start if inactive
    fn verify_or_start_daemon() -> (bool, String) {
        let socket_path = std::env::var("HGB_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
                PathBuf::from(runtime).join("hgb.sock")
            });

        if socket_path.exists() {
            return (true, "Active (socket verified)".to_string());
        }

        // Check systemctl
        let check = Command::new("systemctl")
            .args(["--user", "is-active", "hgbd"])
            .output();

        if let Ok(out) = check {
            if String::from_utf8_lossy(&out.stdout).trim() == "active" {
                return (true, "Active (systemd unit)".to_string());
            }
        }

        // Attempt starting systemctl unit
        let start = Command::new("systemctl")
            .args(["--user", "start", "hgbd"])
            .output();

        if let Ok(out) = start {
            if out.status.success() {
                return (true, "Started via systemctl --user start hgbd".to_string());
            }
        }

        (false, "Offline (run 'hgbd' to start daemon)".to_string())
    }

    /// Render stylized ASCII onboarding dashboard
    pub fn render_dashboard(report: &VibeOnboardingReport) -> String {
        let ws_str = report.workspace_root.display().to_string();
        let git_status = if report.git_initialized {
            "Initialized fresh repository (git init) ✔".green().bold()
        } else {
            "Existing git repository detected ✔".cyan()
        };

        let wrangler_status = if report.wrangler_toml_written {
            "Configured edge KV & Durable Objects (wrangler.toml) ✔".green().bold()
        } else {
            "Existing wrangler.toml preserved ✔".cyan()
        };

        let mcp_status = if report.mcp_json_written {
            "Configured standard MCP tools & SSE (hagibis.mcp.json) ✔".green().bold()
        } else {
            "Existing hagibis.mcp.json preserved ✔".cyan()
        };

        let daemon_status = if report.daemon_running {
            format!("{} ({})", "Active ✔".green().bold(), report.daemon_action_taken.dimmed())
        } else {
            format!("{} ({})", "Standby ⚠️".yellow().bold(), report.daemon_action_taken.dimmed())
        };

        format!(
            "╭────────────────────────────────────────────────────────────────────────╮\n\
             │ {} │\n\
             ├────────────────────────────────────────────────────────────────────────┤\n\
             │ • Workspace Root:  {:<50} │\n\
             │ • Git Repository:  {:<50} │\n\
             │ • Edge Deployment: {:<50} │\n\
             │ • MCP Tools Suite: {:<50} │\n\
             │ • Daemon (hgbd):   {:<50} │\n\
             ├────────────────────────────────────────────────────────────────────────┤\n\
             │ {} │\n\
             │                                                                        │\n\
             │   1. Run zero-friction vibe prompt:                                    │\n\
             │      {}                                     │\n\
             │                                                                        │\n\
             │   2. Launch real-time visual cockpit:                                  │\n\
             │      {}                                                     │\n\
             │                                                                        │\n\
             │   3. Live hardware & governor status:                                  │\n\
             │      {}                                           │\n\
             ╰────────────────────────────────────────────────────────────────────────╯",
            "🪽 HAGIBIS 1-CLICK VIBE CODING ONBOARDING COMPLETE".bright_purple().bold(),
            ws_str.bright_white(),
            git_status,
            wrangler_status,
            mcp_status,
            daemon_status,
            "🚀 Ready for Frictionless Vibe Coding! Next Steps:".bold().white(),
            "$ hgb vibe \"add payment webhook handler\"".bright_cyan().bold(),
            "$ hgb cockpit --vibe".bright_cyan().bold(),
            "$ hgb doctor --hardware".bright_cyan().bold()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_vibe_onboarding_scaffolding_and_files() {
        let temp = tempdir().unwrap();
        let target = temp.path().join("my-test-vibe-project");

        let report = VibeOnboardingEngine::execute_onboarding(Some(&target)).expect("Onboarding should succeed");

        assert!(target.join(".hgb").exists());
        assert!(target.join(".hgb/patches").exists());
        assert!(target.join(".hgb/timelines").exists());
        assert!(target.join(".hgb/mcp_plugins").exists());
        assert!(target.join(".hgb/distillation").exists());
        assert!(target.join("src").exists());

        // Verify wrangler.toml
        assert!(target.join("wrangler.toml").exists());
        let wrangler_content = fs::read_to_string(target.join("wrangler.toml")).unwrap();
        assert!(wrangler_content.contains("HGB_KV_STORAGE"));
        assert!(wrangler_content.contains("HGB_DURABLE_SESSION"));
        assert!(report.wrangler_toml_written);

        // Verify hagibis.mcp.json
        assert!(target.join("hagibis.mcp.json").exists());
        let mcp_content = fs::read_to_string(target.join("hagibis.mcp.json")).unwrap();
        assert!(mcp_content.contains("hgb_self_healing_compile"));
        assert!(report.mcp_json_written);

        // Verify dashboard rendering
        let dashboard = VibeOnboardingEngine::render_dashboard(&report);
        assert!(dashboard.contains("HAGIBIS 1-CLICK VIBE CODING ONBOARDING COMPLETE"));
        assert!(dashboard.contains("hgb vibe"));
        assert!(dashboard.contains("hgb cockpit --vibe"));
    }
}
