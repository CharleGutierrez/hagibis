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

/// Supported modern application frameworks for instant scaffolding
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FrameworkType {
    NextJs,
    SvelteKit,
    ViteReact,
    Axum,
    FastApi,
}

impl FrameworkType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NextJs => "Next.js 15 (App Router, React 19)",
            Self::SvelteKit => "SvelteKit 2 (Svelte 5 Runes)",
            Self::ViteReact => "Vite React (TypeScript + Tailwind CSS)",
            Self::Axum => "Axum (Rust High-Performance Web)",
            Self::FastApi => "FastAPI (Python Async Web)",
        }
    }

    pub fn detect_from_prompt(prompt: &str) -> Self {
        let p = prompt.to_lowercase();
        if p.contains("next") || p.contains("nextjs") || p.contains("next.js") || p.contains("server component") {
            Self::NextJs
        } else if p.contains("svelte") || p.contains("sveltekit") {
            Self::SvelteKit
        } else if p.contains("axum") || p.contains("rust web") || p.contains("rust api") || p.contains("tokio web") || p.contains("rust backend") {
            Self::Axum
        } else if p.contains("fastapi") || p.contains("python api") || p.contains("python backend") || p.contains("uvicorn") {
            Self::FastApi
        } else if p.contains("vite") || p.contains("react") || p.contains("spa") || p.contains("client") {
            Self::ViteReact
        } else {
            Self::NextJs
        }
    }

    pub fn from_override(name: &str) -> Option<Self> {
        let n = name.to_lowercase();
        if n.contains("next") {
            Some(Self::NextJs)
        } else if n.contains("svelte") {
            Some(Self::SvelteKit)
        } else if n.contains("vite") || n.contains("react") {
            Some(Self::ViteReact)
        } else if n.contains("axum") || n.contains("rust") {
            Some(Self::Axum)
        } else if n.contains("fastapi") || n.contains("python") {
            Some(Self::FastApi)
        } else {
            None
        }
    }

    pub fn dev_command(&self) -> &'static str {
        match self {
            Self::NextJs => "npm run dev",
            Self::SvelteKit => "npm run dev",
            Self::ViteReact => "npm run dev",
            Self::Axum => "cargo run",
            Self::FastApi => "uvicorn main:app --reload",
        }
    }
}

/// Detailed outcome of the instant application scaffolding process
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaffoldReport {
    pub prompt: String,
    pub project_name: String,
    pub framework: FrameworkType,
    pub target_dir: PathBuf,
    pub files_created: Vec<String>,
    pub git_initialized: bool,
    pub dev_command: String,
}

/// Instant App Scaffolding Engine (`hgb new "<prompt>"`)
pub struct AppScaffolder;

impl AppScaffolder {
    pub fn slugify(prompt: &str) -> String {
        let mut cleaned: String = prompt
            .chars()
            .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
            .collect();

        while cleaned.contains("--") {
            cleaned = cleaned.replace("--", "-");
        }

        let trimmed = cleaned.trim_matches('-').to_string();
        if trimmed.is_empty() {
            "vibe-app".to_string()
        } else if trimmed.len() > 28 {
            trimmed[..28].trim_end_matches('-').to_string()
        } else {
            trimmed
        }
    }

    pub const CURSOR_MCP_JSON: &'static str = r#"{
  "mcpServers": {
    "hagibis": {
      "command": "hgb",
      "args": ["mcp"],
      "env": {
        "HGB_WORKSPACE": "."
      }
    }
  }
}
"#;

    pub const STANDARD_GITIGNORE: &'static str = r#"# Dependencies
node_modules/
target/
__pycache__/
*.py[cod]
.venv/
env/

# Build outputs
.next/
dist/
build/
out/
*.log

# Hagibis local cache
.hgb/worktrees/
.hgb/patches/
.hgb/active_try.json

# Environment variables
.env
.env.local
.env.*.local
"#;

    /// Execute instant project scaffolding
    pub fn scaffold(
        prompt: &str,
        target_dir_opt: Option<&Path>,
        framework_override: Option<&str>,
    ) -> Result<ScaffoldReport> {
        let framework = framework_override
            .and_then(FrameworkType::from_override)
            .unwrap_or_else(|| FrameworkType::detect_from_prompt(prompt));

        let project_name = if let Some(dir) = target_dir_opt {
            dir.file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_else(|| Self::slugify(prompt))
        } else {
            Self::slugify(prompt)
        };

        let target_dir = if let Some(dir) = target_dir_opt {
            dir.to_path_buf()
        } else {
            std::env::current_dir()?.join(&project_name)
        };

        fs::create_dir_all(&target_dir)?;

        let mut files_created = Vec::new();

        // 1. Common configuration: .cursor/mcp.json and .gitignore
        let cursor_dir = target_dir.join(".cursor");
        fs::create_dir_all(&cursor_dir)?;
        fs::write(cursor_dir.join("mcp.json"), Self::CURSOR_MCP_JSON)?;
        files_created.push(".cursor/mcp.json".to_string());

        fs::write(target_dir.join(".gitignore"), Self::STANDARD_GITIGNORE)?;
        files_created.push(".gitignore".to_string());

        // 2. Write .hgb/config.json
        let hgb_dir = target_dir.join(".hgb");
        fs::create_dir_all(&hgb_dir)?;
        let hgb_cfg = serde_json::json!({
            "generator": "hagibis-app-scaffolder",
            "framework": framework.as_str(),
            "prompt": prompt,
            "created_at": chrono::Utc::now().to_rfc3339()
        });
        fs::write(hgb_dir.join("config.json"), serde_json::to_string_pretty(&hgb_cfg).unwrap())?;
        files_created.push(".hgb/config.json".to_string());

        // 3. Framework-specific scaffolding
        match framework {
            FrameworkType::NextJs => {
                Self::scaffold_nextjs(&target_dir, &project_name, prompt, &mut files_created)?;
            }
            FrameworkType::SvelteKit => {
                Self::scaffold_sveltekit(&target_dir, &project_name, prompt, &mut files_created)?;
            }
            FrameworkType::ViteReact => {
                Self::scaffold_vite_react(&target_dir, &project_name, prompt, &mut files_created)?;
            }
            FrameworkType::Axum => {
                Self::scaffold_axum(&target_dir, &project_name, prompt, &mut files_created)?;
            }
            FrameworkType::FastApi => {
                Self::scaffold_fastapi(&target_dir, &project_name, prompt, &mut files_created)?;
            }
        }

        // 4. Initialize Git repository
        let git_dir = target_dir.join(".git");
        let git_initialized = if !git_dir.exists() {
            Command::new("git")
                .args(["init"])
                .current_dir(&target_dir)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        } else {
            true
        };

        Ok(ScaffoldReport {
            prompt: prompt.to_string(),
            project_name,
            framework,
            target_dir,
            files_created,
            git_initialized,
            dev_command: framework.dev_command().to_string(),
        })
    }

    fn scaffold_nextjs(dir: &Path, name: &str, prompt: &str, files: &mut Vec<String>) -> Result<()> {
        let app_dir = dir.join("app");
        fs::create_dir_all(&app_dir)?;

        // package.json
        let pkg = serde_json::json!({
            "name": name,
            "version": "0.1.0",
            "private": true,
            "scripts": {
                "dev": "next dev",
                "build": "next build",
                "start": "next start",
                "lint": "next lint"
            },
            "dependencies": {
                "lucide-react": "^0.460.0",
                "next": "^15.0.0",
                "react": "^19.0.0",
                "react-dom": "^19.0.0"
            },
            "devDependencies": {
                "@types/node": "^22.0.0",
                "@types/react": "^19.0.0",
                "@types/react-dom": "^19.0.0",
                "autoprefixer": "^10.4.20",
                "postcss": "^8.4.47",
                "tailwindcss": "^3.4.14",
                "typescript": "^5.6.3"
            }
        });
        fs::write(dir.join("package.json"), serde_json::to_string_pretty(&pkg).unwrap())?;
        files.push("package.json".to_string());

        // tsconfig.json
        let tsconfig = serde_json::json!({
            "compilerOptions": {
                "target": "ES2022",
                "lib": ["dom", "dom.iterable", "esnext"],
                "allowJs": true,
                "skipLibCheck": true,
                "strict": true,
                "noEmit": true,
                "esModuleInterop": true,
                "module": "esnext",
                "moduleResolution": "bundler",
                "resolveJsonModule": true,
                "isolatedModules": true,
                "jsx": "preserve",
                "incremental": true,
                "plugins": [{ "name": "next" }],
                "paths": { "@/*": ["./*"] }
            },
            "include": ["next-env.d.ts", "**/*.ts", "**/*.tsx", ".next/types/**/*.ts"],
            "exclude": ["node_modules"]
        });
        fs::write(dir.join("tsconfig.json"), serde_json::to_string_pretty(&tsconfig).unwrap())?;
        files.push("tsconfig.json".to_string());

        // tailwind.config.ts
        let tailwind_config = r#"import type { Config } from "tailwindcss";

const config: Config = {
  darkMode: "class",
  content: [
    "./pages/**/*.{js,ts,jsx,tsx,mdx}",
    "./components/**/*.{js,ts,jsx,tsx,mdx}",
    "./app/**/*.{js,ts,jsx,tsx,mdx}",
  ],
  theme: {
    extend: {
      colors: {
        background: "var(--background)",
        foreground: "var(--foreground)",
      },
    },
  },
  plugins: [],
};
export default config;
"#;
        fs::write(dir.join("tailwind.config.ts"), tailwind_config)?;
        files.push("tailwind.config.ts".to_string());

        // postcss.config.mjs
        let postcss_config = r#"const config = {
  plugins: {
    tailwindcss: {},
    autoprefixer: {},
  },
};
export default config;
"#;
        fs::write(dir.join("postcss.config.mjs"), postcss_config)?;
        files.push("postcss.config.mjs".to_string());

        // app/globals.css
        let globals_css = r#"@tailwind base;
@tailwind components;
@tailwind utilities;

:root {
  --background: #ffffff;
  --foreground: #09090b;
}

.dark {
  --background: #09090b;
  --foreground: #ededed;
}

body {
  color: var(--foreground);
  background: var(--background);
  font-family: system-ui, -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
}
"#;
        fs::write(app_dir.join("globals.css"), globals_css)?;
        files.push("app/globals.css".to_string());

        // app/layout.tsx
        let layout_tsx = format!(r#"import type {{ Metadata }} from "next";
import "./globals.css";

export const metadata: Metadata = {{
  title: "{name}",
  description: "Scaffolded with Hagibis Vibe Engine: {prompt}",
}};

export default function RootLayout({{
  children,
}}: Readonly<{{
  children: React.ReactNode;
}}>) {{
  return (
    <html lang="en" className="dark">
      <body className="min-h-screen bg-zinc-950 text-zinc-100 antialiased selection:bg-purple-500 selection:text-white">
        {{children}}
      </body>
    </html>
  );
}}
"#);
        fs::write(app_dir.join("layout.tsx"), layout_tsx)?;
        files.push("app/layout.tsx".to_string());

        // app/page.tsx
        let page_tsx = format!(r##"'use client';

import React, {{ useState }} from 'react';
import {{ Sparkles, Activity, Terminal, ArrowRight, Sun, Moon, CheckCircle2 }} from 'lucide-react';

export default function Home() {{
  const [isDark, setIsDark] = useState(true);

  const toggleTheme = () => {{
    setIsDark(!isDark);
    document.documentElement.classList.toggle('dark');
  }};

  return (
    <main className="min-h-screen p-8 max-w-5xl mx-auto flex flex-col justify-between">
      <header className="flex justify-between items-center border-b border-zinc-800 pb-6">
        <div className="flex items-center gap-3">
          <div className="w-10 h-10 rounded-xl bg-gradient-to-tr from-purple-600 to-indigo-500 flex items-center justify-center text-white shadow-lg shadow-purple-500/20">
            <Sparkles className="w-5 h-5" />
          </div>
          <div>
            <h1 className="text-xl font-bold tracking-tight text-white capitalize">{name}</h1>
            <p className="text-xs text-zinc-400">Next.js 15 App Router • TypeScript • Tailwind CSS</p>
          </div>
        </div>
        <button
          onClick={{toggleTheme}}
          className="p-2.5 rounded-lg border border-zinc-800 bg-zinc-900 text-zinc-300 hover:text-white hover:bg-zinc-800 transition"
        >
          {{isDark ? <Sun className="w-4 h-4" /> : <Moon className="w-4 h-4" />}}
        </button>
      </header>

      <div className="py-16 flex flex-col items-center text-center">
        <div className="inline-flex items-center gap-2 px-3 py-1 rounded-full border border-purple-500/30 bg-purple-500/10 text-purple-300 text-xs font-medium mb-6">
          <CheckCircle2 className="w-3.5 h-3.5" /> First-Green Instant Scaffolding Ready
        </div>
        <h2 className="text-4xl md:text-5xl font-extrabold tracking-tight max-w-2xl text-transparent bg-clip-text bg-gradient-to-r from-zinc-100 via-zinc-200 to-zinc-400">
          {prompt}
        </h2>
        <p className="mt-4 text-zinc-400 max-w-lg text-sm md:text-base leading-relaxed">
          Pre-seeded with Lucide icons, Dark/Light mode theme system, and complete Cursor MCP integration.
        </p>

        <div className="mt-8 flex gap-4">
          <a
            href="#features"
            className="flex items-center gap-2 px-5 py-2.5 rounded-xl bg-purple-600 hover:bg-purple-500 text-white font-medium text-sm transition shadow-lg shadow-purple-600/30"
          >
            Get Started <ArrowRight className="w-4 h-4" />
          </a>
          <button className="flex items-center gap-2 px-5 py-2.5 rounded-xl border border-zinc-800 bg-zinc-900/60 hover:bg-zinc-900 text-zinc-300 text-sm font-medium transition">
            <Terminal className="w-4 h-4" /> Run Vibe Loop
          </button>
        </div>
      </div>

      <div className="grid grid-cols-1 md:grid-cols-3 gap-6 pt-6">
        <div className="p-6 rounded-2xl border border-zinc-800/80 bg-zinc-900/30 backdrop-blur">
          <Activity className="w-5 h-5 text-purple-400 mb-3" />
          <h3 className="font-semibold text-white mb-1">Live Vibe Telemetry</h3>
          <p className="text-xs text-zinc-400">Integrated with Hagibis daemon and autonomous self-healing check runners.</p>
        </div>
        <div className="p-6 rounded-2xl border border-zinc-800/80 bg-zinc-900/30 backdrop-blur">
          <Sparkles className="w-5 h-5 text-indigo-400 mb-3" />
          <h3 className="font-semibold text-white mb-1">Cursor & MCP Ready</h3>
          <p className="text-xs text-zinc-400">Configured with .cursor/mcp.json for instant sovereign AI execution.</p>
        </div>
        <div className="p-6 rounded-2xl border border-zinc-800/80 bg-zinc-900/30 backdrop-blur">
          <Terminal className="w-5 h-5 text-emerald-400 mb-3" />
          <h3 className="font-semibold text-white mb-1">Speculative Prototyping</h3>
          <p className="text-xs text-zinc-400">Test what-if branches in zero-risk worktrees via $ hgb try.</p>
        </div>
      </div>

      <footer className="mt-16 text-center text-xs text-zinc-600 border-t border-zinc-900 pt-6">
        Scaffolded in seconds with Hagibis Vibe Engine • Edit <code className="text-zinc-400">app/page.tsx</code> to customize
      </footer>
    </main>
  );
}}
"##);
        fs::write(app_dir.join("page.tsx"), page_tsx)?;
        files.push("app/page.tsx".to_string());

        Ok(())
    }

    fn scaffold_sveltekit(dir: &Path, name: &str, prompt: &str, files: &mut Vec<String>) -> Result<()> {
        let src_dir = dir.join("src");
        let routes_dir = src_dir.join("routes");
        fs::create_dir_all(&routes_dir)?;

        // package.json
        let pkg = serde_json::json!({
            "name": name,
            "version": "0.1.0",
            "type": "module",
            "scripts": {
                "dev": "vite dev",
                "build": "vite build",
                "preview": "vite preview"
            },
            "devDependencies": {
                "@sveltejs/adapter-auto": "^3.0.0",
                "@sveltejs/kit": "^2.0.0",
                "@sveltejs/vite-plugin-svelte": "^4.0.0",
                "autoprefixer": "^10.4.20",
                "lucide-svelte": "^0.460.0",
                "postcss": "^8.4.47",
                "svelte": "^5.0.0",
                "tailwindcss": "^3.4.14",
                "typescript": "^5.6.3",
                "vite": "^5.4.0"
            }
        });
        fs::write(dir.join("package.json"), serde_json::to_string_pretty(&pkg).unwrap())?;
        files.push("package.json".to_string());

        // svelte.config.js
        let svelte_cfg = r#"import adapter from '@sveltejs/adapter-auto';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
const config = {
  preprocess: vitePreprocess(),
  kit: {
    adapter: adapter(),
  },
};
export default config;
"#;
        fs::write(dir.join("svelte.config.js"), svelte_cfg)?;
        files.push("svelte.config.js".to_string());

        // vite.config.ts
        let vite_cfg = r#"import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [sveltekit()]
});
"#;
        fs::write(dir.join("vite.config.ts"), vite_cfg)?;
        files.push("vite.config.ts".to_string());

        // src/app.html
        let app_html = r#"<!doctype html>
<html lang="en" class="dark">
  <head>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
    %sveltekit.head%
  </head>
  <body class="min-h-screen bg-zinc-950 text-zinc-100 antialiased">
    <div style="display: contents">%sveltekit.body%</div>
  </body>
</html>
"#;
        fs::write(src_dir.join("app.html"), app_html)?;
        files.push("src/app.html".to_string());

        // src/app.css
        let app_css = r#"@tailwind base;
@tailwind components;
@tailwind utilities;
"#;
        fs::write(src_dir.join("app.css"), app_css)?;
        files.push("src/app.css".to_string());

        // src/routes/+layout.svelte
        let layout_svelte = r#"<script lang="ts">
  import '../app.css';
  let { children } = $props();
</script>

{@render children()}
"#;
        fs::write(routes_dir.join("+layout.svelte"), layout_svelte)?;
        files.push("src/routes/+layout.svelte".to_string());

        // src/routes/+page.svelte
        let page_svelte = format!(r#"<script lang="ts">
  import {{ Sparkles, Activity, Terminal, ArrowRight, Sun, Moon }} from 'lucide-svelte';
  let isDark = $state(true);

  function toggleTheme() {{
    isDark = !isDark;
    if (typeof document !== 'undefined') {{
      document.documentElement.classList.toggle('dark');
    }}
  }}
</script>

<main class="min-h-screen p-8 max-w-5xl mx-auto flex flex-col justify-between">
  <header class="flex justify-between items-center border-b border-zinc-800 pb-6">
    <div class="flex items-center gap-3">
      <div class="w-10 h-10 rounded-xl bg-gradient-to-tr from-amber-500 to-orange-500 flex items-center justify-center text-white shadow-lg shadow-orange-500/20">
        <Sparkles class="w-5 h-5" />
      </div>
      <div>
        <h1 class="text-xl font-bold tracking-tight text-white capitalize">{name}</h1>
        <p class="text-xs text-zinc-400">SvelteKit 2 • Svelte 5 Runes • Tailwind CSS</p>
      </div>
    </div>
    <button
      onclick={{toggleTheme}}
      class="p-2.5 rounded-lg border border-zinc-800 bg-zinc-900 text-zinc-300 hover:text-white transition"
    >
      {{#if isDark}}
        <Sun class="w-4 h-4" />
      {{:else}}
        <Moon class="w-4 h-4" />
      {{/if}}
    </button>
  </header>

  <div class="py-16 flex flex-col items-center text-center">
    <h2 class="text-4xl md:text-5xl font-extrabold tracking-tight max-w-2xl text-transparent bg-clip-text bg-gradient-to-r from-orange-400 via-amber-200 to-zinc-100">
      {prompt}
    </h2>
    <p class="mt-4 text-zinc-400 max-w-lg text-sm md:text-base leading-relaxed">
      SvelteKit application scaffolded with Lucide icons and Dark/Light theme mode.
    </p>
  </div>
</main>
"#);
        fs::write(routes_dir.join("+page.svelte"), page_svelte)?;
        files.push("src/routes/+page.svelte".to_string());

        Ok(())
    }

    fn scaffold_vite_react(dir: &Path, name: &str, prompt: &str, files: &mut Vec<String>) -> Result<()> {
        let src_dir = dir.join("src");
        fs::create_dir_all(&src_dir)?;

        // package.json
        let pkg = serde_json::json!({
            "name": name,
            "version": "0.1.0",
            "private": true,
            "type": "module",
            "scripts": {
                "dev": "vite",
                "build": "tsc && vite build",
                "preview": "vite preview"
            },
            "dependencies": {
                "lucide-react": "^0.460.0",
                "react": "^18.3.1",
                "react-dom": "^18.3.1"
            },
            "devDependencies": {
                "@types/react": "^18.3.12",
                "@types/react-dom": "^18.3.1",
                "@vitejs/plugin-react": "^4.3.3",
                "autoprefixer": "^10.4.20",
                "postcss": "^8.4.47",
                "tailwindcss": "^3.4.14",
                "typescript": "^5.6.3",
                "vite": "^5.4.10"
            }
        });
        fs::write(dir.join("package.json"), serde_json::to_string_pretty(&pkg).unwrap())?;
        files.push("package.json".to_string());

        // vite.config.ts
        let vite_cfg = r#"import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
});
"#;
        fs::write(dir.join("vite.config.ts"), vite_cfg)?;
        files.push("vite.config.ts".to_string());

        // index.html
        let index_html = format!(r#"<!doctype html>
<html lang="en" class="dark">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>{name}</title>
  </head>
  <body class="min-h-screen bg-zinc-950 text-zinc-100 antialiased">
    <div id="root"></div>
    <script type="module" src="/src/main.tsx"></script>
  </body>
</html>
"#);
        fs::write(dir.join("index.html"), index_html)?;
        files.push("index.html".to_string());

        // src/index.css
        let index_css = r#"@tailwind base;
@tailwind components;
@tailwind utilities;
"#;
        fs::write(src_dir.join("index.css"), index_css)?;
        files.push("src/index.css".to_string());

        // src/main.tsx
        let main_tsx = r#"import React from 'react';
import ReactDOM from 'react-dom/client';
import App from './App';
import './index.css';

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
"#;
        fs::write(src_dir.join("main.tsx"), main_tsx)?;
        files.push("src/main.tsx".to_string());

        // src/App.tsx
        let app_tsx = format!(r#"import React, {{ useState }} from 'react';
import {{ Sparkles, Activity, Sun, Moon, ArrowRight }} from 'lucide-react';

export default function App() {{
  const [isDark, setIsDark] = useState(true);

  const toggleTheme = () => {{
    setIsDark(!isDark);
    document.documentElement.classList.toggle('dark');
  }};

  return (
    <div className="min-h-screen p-8 max-w-5xl mx-auto flex flex-col justify-between">
      <header className="flex justify-between items-center border-b border-zinc-800 pb-6">
        <div className="flex items-center gap-3">
          <div className="w-10 h-10 rounded-xl bg-gradient-to-tr from-cyan-500 to-blue-500 flex items-center justify-center text-white shadow-lg shadow-cyan-500/20">
            <Sparkles className="w-5 h-5" />
          </div>
          <div>
            <h1 className="text-xl font-bold tracking-tight text-white capitalize">{name}</h1>
            <p className="text-xs text-zinc-400">Vite React • TypeScript • Tailwind CSS</p>
          </div>
        </div>
        <button
          onClick={{toggleTheme}}
          className="p-2.5 rounded-lg border border-zinc-800 bg-zinc-900 text-zinc-300 hover:text-white transition"
        >
          {{isDark ? <Sun className="w-4 h-4" /> : <Moon className="w-4 h-4" />}}
        </button>
      </header>

      <div className="py-16 flex flex-col items-center text-center">
        <h2 className="text-4xl md:text-5xl font-extrabold tracking-tight max-w-2xl text-transparent bg-clip-text bg-gradient-to-r from-cyan-400 via-blue-200 to-zinc-100">
          {prompt}
        </h2>
        <p className="mt-4 text-zinc-400 max-w-lg text-sm md:text-base leading-relaxed">
          Pre-seeded Vite React app with Lucide icons, Dark/Light mode theme system, and Cursor MCP integration.
        </p>
      </div>
    </div>
  );
}}
"#);
        fs::write(src_dir.join("App.tsx"), app_tsx)?;
        files.push("src/App.tsx".to_string());

        Ok(())
    }

    fn scaffold_axum(dir: &Path, name: &str, prompt: &str, files: &mut Vec<String>) -> Result<()> {
        let src_dir = dir.join("src");
        fs::create_dir_all(&src_dir)?;

        // Cargo.toml
        let cargo_toml = format!(r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[dependencies]
axum = "0.7"
tokio = {{ version = "1.0", features = ["full"] }}
serde = {{ version = "1.0", features = ["derive"] }}
serde_json = "1.0"
tower = "0.5"
tower-http = {{ version = "0.6", features = ["cors", "trace"] }}
tracing = "0.1"
tracing-subscriber = "0.3"
"#);
        fs::write(dir.join("Cargo.toml"), cargo_toml)?;
        files.push("Cargo.toml".to_string());

        // src/main.rs
        let main_rs = format!(r#"//! {name} - High performance Axum web server
//! Scaffolded with Hagibis Vibe Engine: {prompt}

use axum::{{
    routing::{{get, post}},
    extract::Json,
    response::IntoResponse,
    Router,
}};
use serde::{{Deserialize, Serialize}};
use std::net::SocketAddr;
use tower_http::cors::{{Any, CorsLayer}};

#[derive(Serialize)]
struct StatusResponse {{
    name: &'static str,
    status: &'static str,
    prompt: &'static str,
    version: &'static str,
}}

#[derive(Deserialize, Serialize)]
struct VibePayload {{
    intent: String,
    metadata: Option<String>,
}}

async fn root() -> impl IntoResponse {{
    Json(StatusResponse {{
        name: "{name}",
        status: "ok",
        prompt: "{prompt}",
        version: "0.1.0",
    }})
}}

async fn health() -> impl IntoResponse {{
    Json(serde_json::json!({{ "status": "healthy", "timestamp": chrono::Utc::now().to_rfc3339() }}))
}}

async fn handle_vibe(Json(payload): Json<VibePayload>) -> impl IntoResponse {{
    Json(serde_json::json!({{
        "received": payload.intent,
        "action": "autonomous_verified",
        "first_green": true,
    }}))
}}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {{
    tracing_subscriber::fmt::init();

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/api/vibe", post(handle_vibe))
        .layer(cors);

    let addr = SocketAddr::from(([0, 0, 0, 0], 3000));
    println!("🦀 Axum server running on http://{{}}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}}
"#);
        fs::write(src_dir.join("main.rs"), main_rs)?;
        files.push("src/main.rs".to_string());

        Ok(())
    }

    fn scaffold_fastapi(dir: &Path, name: &str, prompt: &str, files: &mut Vec<String>) -> Result<()> {
        // requirements.txt
        let reqs = r#"fastapi>=0.115.0
uvicorn[standard]>=0.32.0
pydantic>=2.9.0
"#;
        fs::write(dir.join("requirements.txt"), reqs)?;
        files.push("requirements.txt".to_string());

        // main.py
        let main_py = format!(r#"# {name} - High performance FastAPI async server
# Scaffolded with Hagibis Vibe Engine: {prompt}

from fastapi import FastAPI
from fastapi.middleware.cors import CORSMiddleware
from pydantic import BaseModel
from typing import Optional
from datetime import datetime

app = FastAPI(
    title="{name}",
    description="Scaffolded with Hagibis: {prompt}",
    version="0.1.0"
)

app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)

class VibeRequest(BaseModel):
    intent: str
    metadata: Optional[str] = None

@app.get("/")
def read_root():
    return {{
        "name": "{name}",
        "status": "ok",
        "prompt": "{prompt}",
        "timestamp": datetime.utcnow().isoformat()
    }}

@app.get("/health")
def read_health():
    return {{"status": "healthy"}}

@app.post("/api/vibe")
def post_vibe(payload: VibeRequest):
    return {{
        "received": payload.intent,
        "action": "autonomous_verified",
        "first_green": True
    }}

if __name__ == "__main__":
    import uvicorn
    uvicorn.run("main:app", host="0.0.0.0", port=8000, reload=True)
"#);
        fs::write(dir.join("main.py"), main_py)?;
        files.push("main.py".to_string());

        Ok(())
    }

    /// Render stylized ASCII scaffolding dashboard
    pub fn render_scaffold_card(report: &ScaffoldReport) -> String {
        format!(
            "╭────────────────────────────────────────────────────────────────────────╮\n\
             │ {} │\n\
             ├────────────────────────────────────────────────────────────────────────┤\n\
             │ • Project Name:    {:<50} │\n\
             │ • Framework:       {:<50} │\n\
             │ • Destination:     {:<50} │\n\
             │ • Files Created:   {:<50} │\n\
             │ • Git Repository:  {:<50} │\n\
             │ • MCP Integration: {:<50} │\n\
             ├────────────────────────────────────────────────────────────────────────┤\n\
             │ {} │\n\
             │                                                                        │\n\
             │   1. Enter project directory:                                          │\n\
             │      {} {:<45} │\n\
             │                                                                        │\n\
             │   2. Start development server:                                         │\n\
             │      {} {:<45} │\n\
             │                                                                        │\n\
             │   3. Launch speculative what-if prototype:                             │\n\
             │      {} {:<45} │\n\
             ╰────────────────────────────────────────────────────────────────────────╯",
            "✨ INSTANT APP SCAFFOLDING COMPLETE".bright_green().bold(),
            report.project_name.bright_white().bold(),
            report.framework.as_str().cyan(),
            report.target_dir.display().to_string().bright_white(),
            format!("{} files pre-seeded", report.files_created.len()).green(),
            if report.git_initialized { "Initialized (git init) ✔".green().bold() } else { "Skipped".yellow() },
            "Configured (.cursor/mcp.json) ✔".green().bold(),
            "🚀 Ready for Frictionless Development! Next Steps:".bold().white(),
            "$ cd".bright_cyan().bold(),
            report.project_name.bold(),
            "$".bright_cyan().bold(),
            report.dev_command.bright_yellow().bold(),
            "$ hgb try".bright_cyan().bold(),
            "\"add auth flow\"".bright_white()
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

    #[test]
    fn test_framework_detection_from_prompt() {
        assert_eq!(FrameworkType::detect_from_prompt("Build Next.js 15 crypto portfolio"), FrameworkType::NextJs);
        assert_eq!(FrameworkType::detect_from_prompt("SvelteKit 2 realtime chat"), FrameworkType::SvelteKit);
        assert_eq!(FrameworkType::detect_from_prompt("Axum high speed rust backend"), FrameworkType::Axum);
        assert_eq!(FrameworkType::detect_from_prompt("FastAPI python data service"), FrameworkType::FastApi);
        assert_eq!(FrameworkType::detect_from_prompt("Vite react single page app"), FrameworkType::ViteReact);
    }

    #[test]
    fn test_scaffold_nextjs_project() {
        let temp = tempdir().unwrap();
        let target = temp.path().join("my-next-app");

        let report = AppScaffolder::scaffold("Next.js 15 SaaS Dashboard with Stripe", Some(&target), None).unwrap();
        assert_eq!(report.framework, FrameworkType::NextJs);
        assert!(target.join("package.json").exists());
        assert!(target.join("tsconfig.json").exists());
        assert!(target.join("tailwind.config.ts").exists());
        assert!(target.join("app/layout.tsx").exists());
        assert!(target.join("app/page.tsx").exists());
        assert!(target.join(".cursor/mcp.json").exists());
        assert!(target.join(".gitignore").exists());

        let pkg = fs::read_to_string(target.join("package.json")).unwrap();
        assert!(pkg.contains("lucide-react"));
        assert!(pkg.contains("next"));

        let page = fs::read_to_string(target.join("app/page.tsx")).unwrap();
        assert!(page.contains("lucide-react"));
        assert!(page.contains("Sparkles"));
    }

    #[test]
    fn test_scaffold_axum_project() {
        let temp = tempdir().unwrap();
        let target = temp.path().join("my-axum-service");

        let report = AppScaffolder::scaffold("Axum microservice with metrics", Some(&target), None).unwrap();
        assert_eq!(report.framework, FrameworkType::Axum);
        assert!(target.join("Cargo.toml").exists());
        assert!(target.join("src/main.rs").exists());
        assert!(target.join(".cursor/mcp.json").exists());

        let main = fs::read_to_string(target.join("src/main.rs")).unwrap();
        assert!(main.contains("Router") && main.contains("axum::"));
        assert!(main.contains("/health"));
        assert!(main.contains("/api/vibe"));
    }

    #[test]
    fn test_scaffold_sveltekit_and_fastapi_and_vite() {
        let temp = tempdir().unwrap();

        // SvelteKit
        let svelte_target = temp.path().join("svelte-app");
        let rep_s = AppScaffolder::scaffold("SvelteKit app", Some(&svelte_target), None).unwrap();
        assert_eq!(rep_s.framework, FrameworkType::SvelteKit);
        assert!(svelte_target.join("package.json").exists());
        assert!(svelte_target.join("src/routes/+page.svelte").exists());

        // FastAPI
        let fastapi_target = temp.path().join("fastapi-app");
        let rep_f = AppScaffolder::scaffold("FastAPI python app", Some(&fastapi_target), None).unwrap();
        assert_eq!(rep_f.framework, FrameworkType::FastApi);
        assert!(fastapi_target.join("requirements.txt").exists());
        assert!(fastapi_target.join("main.py").exists());

        // Vite React
        let vite_target = temp.path().join("vite-app");
        let rep_v = AppScaffolder::scaffold("Vite react client", Some(&vite_target), None).unwrap();
        assert_eq!(rep_v.framework, FrameworkType::ViteReact);
        assert!(vite_target.join("src/App.tsx").exists());
        assert!(vite_target.join("index.html").exists());

        // Render card
        let card = AppScaffolder::render_scaffold_card(&rep_s);
        assert!(card.contains("INSTANT APP SCAFFOLDING COMPLETE"));
        assert!(card.contains("SvelteKit"));
    }
}
